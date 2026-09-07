#pragma once

#include "ipc/view.h"

// Read-side cursor over a host-produced RenderMesh vector. The client never writes
// these vectors; the 64-bit host appends the visible set and the client walks it.

class IpcClientVector {
	IPC::VecView<RenderMesh> m_view;
	bool m_isAtBeginning;
	// Index of the element next() will return. The view's own cursor cannot serve this role:
	// next() returns the element it is already sitting on before advancing, so once iteration
	// has begun the view's cursor names the element already handed out, one behind this.
	std::uint32_t m_nextIndex;

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
