#include "ipc/vecwrap.h"

IpcClientVector::IpcClientVector() : m_view(), m_isAtBeginning(true) {}
IpcClientVector::IpcClientVector(const IPC::VecView<RenderMesh>& view) : m_view(view), m_isAtBeginning(true) {
	restart();
}

IpcClientVector& IpcClientVector::operator=(const IpcClientVector& other) {
	m_view = other.m_view;
	m_isAtBeginning = other.m_isAtBeginning;
	return *this;
}

void IpcClientVector::restart() {
	m_view.set_index(0);
	m_isAtBeginning = true;
}

const RenderMesh& IpcClientVector::first() {
	m_isAtBeginning = true;
	return m_view.front();
}

const RenderMesh& IpcClientVector::next() {
	// Return the current element before advancing so a window remap cannot invalidate the result.
	if (m_isAtBeginning) {
		m_isAtBeginning = false;
		return m_view.front();
	} else {
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
	return m_view.at_end();
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
