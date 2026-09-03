#include "client.h"
#include "mge/dlformat.h"
#include "support/log.h"

#include <algorithm>
#include <cassert>
#include <cstdio>
#include <cstring>

extern bool g_isProcessExiting;

// ideally this would go in beginRpc, but we can't put it there because we
// need to check that the previous command has finished before we start
// manipulating parameters
#define WAIT_FOR_PREVIOUS_COMMAND { \
	if (tryWaitForCompletion() != WakeReason::Complete) { \
		return false; \
	} \
}

namespace {
	bool assignKillOnCloseJob(HANDLE job, HANDLE process) {
		JOBOBJECT_EXTENDED_LIMIT_INFORMATION info = {};
		info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
		if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info, sizeof(info))) {
			LOG::winerror("Failed to configure 64-bit host job object");
			return false;
		}
		if (!AssignProcessToJobObject(job, process)) {
			LOG::winerror("Failed to assign 64-bit host process to job object");
			return false;
		}
		return true;
	}
}

namespace IPC {
	Client::Client() :
		m_process(INVALID_HANDLE_VALUE),
		m_sharedMem(INVALID_HANDLE_VALUE),
		m_rpcStartEvent(INVALID_HANDLE_VALUE),
		m_rpcCompleteEvent(INVALID_HANDLE_VALUE),
		m_job(INVALID_HANDLE_VALUE),
		m_ipcParameters(nullptr),
		m_isRpcPending(false),
		m_launchState(Inactive)
	{}

	Client::~Client() {
		if (!g_isProcessExiting) {
			stopServer();
		}
	}

	bool Client::isServerActive() {
		if (m_process != INVALID_HANDLE_VALUE && m_process != NULL) {
			auto waitResult = WaitForSingleObject(m_process, 0);
			if (waitResult == WAIT_OBJECT_0 || waitResult == WAIT_FAILED) {
				// the host process was started but is no longer running
				stopServer();
				return false;
			}

			return true;
		}

		return false;
	}

	bool Client::launchServer(const char* executable, const char* workingDirectory) {
		STARTUPINFOA startupInfo = {};
		PROCESS_INFORMATION processInfo = {};
		HANDLE thisProcess = INVALID_HANDLE_VALUE;
		char strHandles[256] = { 0, };

		startupInfo.cb = sizeof(startupInfo);

		stopServer();

		// make the mapping handle inheritable
		SECURITY_ATTRIBUTES attrsAllowInherit = {
			sizeof(SECURITY_ATTRIBUTES), NULL, TRUE
		};
		m_sharedMem = CreateFileMappingA(INVALID_HANDLE_VALUE, &attrsAllowInherit, PAGE_READWRITE, 0, sizeof(Parameters), NULL);
		if (m_sharedMem == NULL) {
			LOG::winerror("Failed to create shared memory region");
			goto failedOnLaunch;
		}

		m_ipcParameters = static_cast<Parameters*>(MapViewOfFile(m_sharedMem, FILE_MAP_ALL_ACCESS, 0, 0, 0));
		if (m_ipcParameters == nullptr) {
			LOG::winerror("Failed to map shared memory region");
			goto failedOnLaunch;
		}

		ZeroMemory(m_ipcParameters, sizeof(Parameters));

		if (!DuplicateHandle(GetCurrentProcess(), GetCurrentProcess(), GetCurrentProcess(), &thisProcess, 0, TRUE, DUPLICATE_SAME_ACCESS)) {
			LOG::winerror("Failed to duplicate current process handle");
			goto failedOnLaunch;
		}

		m_rpcStartEvent = CreateEventA(&attrsAllowInherit, FALSE, FALSE, NULL);
		if (m_rpcStartEvent == NULL) {
			LOG::winerror("Failed to create RPC start event");
			goto failedOnLaunch;
		}

		m_rpcCompleteEvent = CreateEventA(&attrsAllowInherit, FALSE, FALSE, NULL);
		if (m_rpcCompleteEvent == NULL) {
			LOG::winerror("Failed to create RPC complete event");
			goto failedOnLaunch;
		}

		int written = std::snprintf(strHandles, sizeof(strHandles), "%p %p %p %p", m_sharedMem, thisProcess, m_rpcStartEvent, m_rpcCompleteEvent);
		if (written <= 0 || static_cast<std::size_t>(written) >= sizeof(strHandles)) {
			LOG::logline("Failed to format 64-bit host startup parameters");
			goto failedOnLaunch;
		}
		if (!CreateProcessA(executable, strHandles, NULL, NULL, TRUE, CREATE_NO_WINDOW, NULL, workingDirectory, &startupInfo, &processInfo)) {
			LOG::winerror("Failed to start 64-bit host process %s", executable);
			goto failedOnLaunch;
		}

		CloseHandle(thisProcess);
		thisProcess = INVALID_HANDLE_VALUE;
		m_process = processInfo.hProcess;
		CloseHandle(processInfo.hThread);
		processInfo.hThread = NULL;

		m_job = CreateJobObjectA(NULL, NULL);
		if (m_job == NULL) {
			LOG::winerror("Failed to create 64-bit host job object");
			goto failedOnLaunch;
		}
		if (!assignKillOnCloseJob(m_job, m_process)) {
			goto failedOnLaunch;
		}

		m_launchState = LaunchedPendingBootstrap;

		LOG::logline("64-bit host process started (PID %u)", processInfo.dwProcessId);
		return true;

	failedOnLaunch:
		if (processInfo.hThread != NULL) {
			CloseHandle(processInfo.hThread);
		}
		if (thisProcess != INVALID_HANDLE_VALUE && thisProcess != NULL) {
			CloseHandle(thisProcess);
		}
		stopServer();
		return false;
	}

	bool Client::startServer(const char* executable, const char* workingDirectory) {
		if (!launchServer(executable, workingDirectory)) {
			return false;
		}

		// wait for the server to finish bootstrapping; this may include final startup generation coordination
		if (waitForBootstrapReady() == WakeReason::Complete) {
			markRuntimeActive();
			return true;
		}

		LOG::logline("Failed waiting for 64-bit host process to initialize");
		return false;
	}

	WakeReason Client::waitForBootstrapReady() {
		if (!isServerActive()) {
			return WakeReason::ServerLost;
		}

		auto result = waitForBootstrapCompletion();
		if (result != WakeReason::Complete) {
			stopServer();
		}

		return result;
	}

	WakeReason Client::pollBootstrapReady() {
		if (!isServerActive()) {
			return WakeReason::ServerLost;
		}

		auto result = waitForCompletion(0);
		if (result == WakeReason::ServerLost || result == WakeReason::Error) {
			stopServer();
		}
		return result;
	}

	WakeReason Client::pollRpcCompletion() {
		return tryWaitForCompletion(0);
	}

	void Client::stopServer() {
		m_isRpcPending = false;

		if (m_process != INVALID_HANDLE_VALUE && m_process != NULL) {
			auto waitResult = WaitForSingleObject(m_process, 0);
			if (waitResult == WAIT_TIMEOUT) {
				TerminateProcess(m_process, 0);
			}
		}

		CleanupHandle(m_job);
		CleanupHandle(m_process);
		CleanupHandle(m_rpcCompleteEvent);
		CleanupHandle(m_rpcStartEvent);
		if (m_ipcParameters != nullptr) {
			UnmapViewOfFile(m_ipcParameters);
			m_ipcParameters = nullptr;
		}
		CleanupHandle(m_sharedMem);
		m_launchState = Inactive;
	}

	bool Client::beginRpc(Command command) {
		if (m_isRpcPending) {
			LOG::logline("Attempted RPC while another RPC was still in progress");
			return false;
		}

		ResetEvent(m_rpcCompleteEvent);

		m_ipcParameters->command = command;
		if (!SetEvent(m_rpcStartEvent)) {
			LOG::winerror("Failed to set RPC start event");
			return false;
		}

		m_isRpcPending = true;
		return true;
	}

	bool Client::allocVec(std::size_t elementSize, std::size_t windowSizeInElements, std::size_t maxSizeInElements, std::size_t initialCapacity) {
		WAIT_FOR_PREVIOUS_COMMAND;

		auto& params = m_ipcParameters->params.allocVecParams;
		params.elementSize = elementSize;
		params.windowSizeInElements = windowSizeInElements;
		params.maxCapacityInElements = maxSizeInElements;
		params.initialCapacity = initialCapacity;
		return beginRpc(Command::AllocVec);
	}

	bool Client::freeVec(VecId id) {
		WAIT_FOR_PREVIOUS_COMMAND;

		m_ipcParameters->params.freeVecParams.id = id;
		return beginRpc(Command::FreeVec);
	}

	bool Client::awaitFreeVec() {
		assert(m_ipcParameters->command == Command::FreeVec);

		if (waitForCompletion() != WakeReason::Complete) {
			LOG::logline("Vec free RPC failed");
			return false;
		}

		return m_ipcParameters->params.freeVecParams.wasFreed;
	}

	bool Client::freeVecBlocking(VecId id) {
		if (!freeVec(id)) {
			return false;
		}

		return awaitFreeVec();
	}

	bool Client::updateDynVis(VecId id) {
		WAIT_FOR_PREVIOUS_COMMAND;

		m_ipcParameters->params.dynVisParams.id = id;
		return beginRpc(Command::UpdateDynVis);
	}

	bool Client::initDistantStatics(VecId distantStatics, VecId distantSubsets, float farStaticMinSize, float veryFarStaticMinSize) {
		WAIT_FOR_PREVIOUS_COMMAND;

		auto& params = m_ipcParameters->params.distantStaticParams;
		params.distantStatics = distantStatics;
		params.distantSubsets = distantSubsets;
		params.farStaticMinSize = farStaticMinSize;
		params.veryFarStaticMinSize = veryFarStaticMinSize;
		params.success = 0;
		return beginRpc(Command::InitDistantStatics);
	}

	bool Client::updateResidency(VecId commits) {
		WAIT_FOR_PREVIOUS_COMMAND;
		auto& params = m_ipcParameters->params.updateResidencyParams;
		params.commits = commits;
		params.success = 0;
		return beginRpc(Command::UpdateResidency);
	}

	bool Client::planResidency(
		VecId plan,
		std::uint32_t planEpoch,
		const D3DXVECTOR3& center,
		float admissionRadius,
		float retainRadius,
		std::uint32_t maxCells,
		std::uint32_t maxResources,
		std::uint32_t viewHeadingBin,
		std::uint64_t capBytes,
		std::uint64_t availableBytes,
		std::uint64_t capDebtBytes
	) {
		WAIT_FOR_PREVIOUS_COMMAND;
		auto& params = m_ipcParameters->params.planResidencyParams;
		params.plan = plan;
		params.planEpoch = planEpoch;
		params.centerX = center.x;
		params.centerY = center.y;
		params.centerZ = center.z;
		params.admissionRadius = admissionRadius;
		params.retainRadius = retainRadius;
		params.maxCells = maxCells;
		params.maxResources = maxResources;
		params.viewHeadingBin = viewHeadingBin;
		params.capBytes = capBytes;
		params.availableBytes = availableBytes;
		params.capDebtBytes = capDebtBytes;
		return beginRpc(Command::PlanResidency);
	}

	bool Client::initLandscape(VecId landscapeBuffers) {
		WAIT_FOR_PREVIOUS_COMMAND;

		auto& params = m_ipcParameters->params.initLandscapeParams;
		params.buffers = landscapeBuffers;
		params.terrainSortToken = TerrainBin::TerrainSortToken;
		params.success = 0;
		return beginRpc(Command::InitLandscape);
	}

	bool Client::queryOutputStatus() {
		WAIT_FOR_PREVIOUS_COMMAND;
		m_ipcParameters->params.outputStatusParams.status = OutputPending;
		return beginRpc(Command::QueryOutputStatus);
	}

	bool Client::waitForOutputReady() {
		while (true) {
			if (!queryOutputStatus()) {
				return false;
			}
			if (waitForCompletion() != WakeReason::Complete) {
				return false;
			}
			switch (outputStatus()) {
			case OutputReady:
				return true;
			case OutputFailed:
				return false;
			case OutputPending:
			default:
				Sleep(10);
				break;
			}
		}
	}

	bool Client::setWorldSpaceBlocking(const std::string& cellname) {
		if (!m_ipcParameters) {
			return false;
		}
		WAIT_FOR_PREVIOUS_COMMAND;

		// The field is not NUL-terminated: a name filling all 64 bytes is valid, and the host
		// reads the whole field when no terminator is present. Forcing one here would make
		// such a name impossible to match.
		auto& params = m_ipcParameters->params.worldSpaceParams;
		memset(params.cellname, 0, sizeof(params.cellname));
		memcpy(params.cellname, cellname.data(), std::min(cellname.size(), sizeof(params.cellname)));
		if (!beginRpc(Command::SetWorldSpace)) {
			return false;
		}

		if (waitForCompletion() != WakeReason::Complete) {
			return false;
		}

		return params.cellFound;
	}

	bool Client::getVisibleMeshesCoarse(VecId visibleSet, const ViewFrustum& viewFrustum, DWORD setFlags, VisibleSetSort sort) {
		WAIT_FOR_PREVIOUS_COMMAND;

		auto& params = m_ipcParameters->params.meshParams;
		params.visibleSet = visibleSet;
		params.viewFrustum = viewFrustum;
		params.sort = sort;
		params.setFlags = setFlags;
		return beginRpc(Command::GetVisibleMeshesCoarse);
	}

	bool Client::getVisibleMeshes(
		VecId visibleSet,
		const ViewFrustum& viewFrustum,
		const D3DXVECTOR4& viewSphere,
		DWORD setFlags,
		float nearStaticEnd,
		float farStaticEnd,
		VisibleSetSort sort
	) {
		WAIT_FOR_PREVIOUS_COMMAND;

		auto& params = m_ipcParameters->params.meshParams;
		params.visibleSet = visibleSet;
		params.viewFrustum = viewFrustum;
		params.viewSphere = viewSphere;
		params.sort = sort;
		params.setFlags = setFlags;
		params.nearStaticEnd = nearStaticEnd;
		params.farStaticEnd = farStaticEnd;
		return beginRpc(Command::GetVisibleMeshes);
	}

	bool Client::sortVisibleSet(VecId visibleSet, VisibleSetSort sort) {
		WAIT_FOR_PREVIOUS_COMMAND;

		if (sort == VisibleSetSort::None) {
			SetEvent(m_rpcCompleteEvent);
			return true;
		}

		auto& params = m_ipcParameters->params.meshParams;
		params.visibleSet = visibleSet;
		params.sort = sort;
		return beginRpc(Command::SortVisibleSet);
	}

	bool Client::setHorizonConfig(const SetHorizonConfigParameters& config) {
		WAIT_FOR_PREVIOUS_COMMAND;

		m_ipcParameters->params.horizonConfigParams = config;
		if (!beginRpc(Command::SetHorizonConfig)) {
			return false;
		}

		return waitForCompletion() == WakeReason::Complete;
	}

	bool Client::finishHorizonFrame() {
		WAIT_FOR_PREVIOUS_COMMAND;

		return beginRpc(Command::FinishHorizonFrame);
	}

	WakeReason Client::waitForCompletion(DWORD ms) {
		auto result = WaitForMultipleObjects(2, m_waitHandles, FALSE, ms);
		switch (result) {
		case WAIT_FAILED:
			LOG::winerror("IPC client wait for RPC completion failed");
			return WakeReason::Error;
		case WAIT_TIMEOUT:
			return WakeReason::Timeout;
		default:
			auto handleIndex = result - WAIT_OBJECT_0;
			switch (handleIndex) {
			case 0:
				return WakeReason::ServerLost;
			case 1:
				m_isRpcPending = false;
				return WakeReason::Complete;
			default:
				return WakeReason::Error;
			}
		}
	}

	WakeReason Client::waitForBootstrapCompletion() {
		const DWORD waitChunkMs = 5000;
		const DWORD logEveryMs = 30000;
		DWORD elapsed = 0;
		DWORD nextLog = logEveryMs;

		while (true) {
			auto reason = waitForCompletion(waitChunkMs);
			if (reason != WakeReason::Timeout) {
				if (reason == WakeReason::Complete && elapsed >= waitChunkMs) {
					LOG::logline("64-bit host process initialized after %u seconds", elapsed / 1000);
				}
				return reason;
			}

			elapsed += waitChunkMs;
			if (elapsed >= nextLog) {
				LOG::logline("Still waiting for 64-bit host process to initialize (%u seconds elapsed)", elapsed / 1000);
				nextLog += logEveryMs;
			}
		}
	}

	WakeReason Client::tryWaitForCompletion(DWORD ms) {
		if (m_isRpcPending) {
			return waitForCompletion(ms);
		}

		return WakeReason::Complete;
	}
}
