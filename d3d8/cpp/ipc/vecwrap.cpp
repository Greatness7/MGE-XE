#include "ipc/vecwrap.h"

IpcClientVector::IpcClientVector() : m_view(), m_isAtBeginning(true), m_nextIndex(0) {}
IpcClientVector::IpcClientVector(const IPC::VecView<RenderMesh>& view) : m_view(view), m_isAtBeginning(true), m_nextIndex(0) {
	restart();
}

IpcClientVector& IpcClientVector::operator=(const IpcClientVector& other) {
	m_view = other.m_view;
	m_isAtBeginning = other.m_isAtBeginning;
	m_nextIndex = other.m_nextIndex;
	return *this;
}

void IpcClientVector::restart() {
	m_view.set_index(0);
	m_isAtBeginning = true;
	m_nextIndex = 0;
}

const RenderMesh& IpcClientVector::first() {
	m_isAtBeginning = true;
	m_nextIndex = 0;
	return m_view.front();
}

const RenderMesh& IpcClientVector::next() {
	// Return the current element before advancing so a window remap cannot invalidate the result.
	if (m_isAtBeginning) {
		m_isAtBeginning = false;
		m_nextIndex = 1;
		return m_view.front();
	} else {
		++m_nextIndex;
		return *++m_view;
	}
}

void IpcClientVector::start_read() {
	m_view.start_read();
}

void IpcClientVector::end_read() {
	m_view.end_read();
}

bool IpcClientVector::at_end() {
	// Test the element next() is about to return, not the one it last returned. The view's
	// at_end tests its own cursor, which lags by one for the whole walk after the first next(),
	// so using it ran one element past the published size and read whatever bytes an earlier,
	// larger population had left at that offset -- a live-looking RenderMesh naming a vertex
	// buffer the streaming path had already released.
	if (m_nextIndex >= m_view.size()) {
		m_view.wait_read();
	}
	return m_nextIndex >= m_view.size();
}

std::uint32_t IpcClientVector::size() const {
	return m_view.size();
}

void IpcClientVector::clear() {
	m_view.clear();
}

void IpcClientVector::truncate(std::uint32_t count) {
	m_view.truncate(count);
}
