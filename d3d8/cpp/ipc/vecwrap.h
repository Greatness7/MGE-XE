#pragma once

#include "ipc/view.h"

// Read-side cursor over a host-produced RenderMesh vector. The client never writes
// these vectors; the 64-bit host appends the visible set and the client walks it.

class IpcClientVector {
	IPC::VecView<RenderMesh> m_view;
	bool m_isAtBeginning;

public:
	IpcClientVector();
	IpcClientVector(const IPC::VecView<RenderMesh>& view);
	IpcClientVector& operator=(const IpcClientVector& other);

	void restart();

	const RenderMesh& first();
	const RenderMesh& next();

	void start_read();

	void end_read();

	bool at_end();

	std::uint32_t size() const;

	void clear();

	void truncate(std::uint32_t count);
};
