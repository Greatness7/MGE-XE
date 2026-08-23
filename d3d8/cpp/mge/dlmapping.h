#pragma once

#include "support/winheader.h"
#include "support/log.h"

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>

class ReadOnlyMappedFile {
public:
    explicit ReadOnlyMappedFile(std::uint64_t defaultWindowBytes = 64ull * 1024ull * 1024ull)
        : defaultWindowBytes(defaultWindowBytes == 0 ? 1 : defaultWindowBytes) {
    }

    ~ReadOnlyMappedFile() {
        reset();
    }

    ReadOnlyMappedFile(const ReadOnlyMappedFile&) = delete;
    ReadOnlyMappedFile& operator=(const ReadOnlyMappedFile&) = delete;
    ReadOnlyMappedFile(ReadOnlyMappedFile&&) = delete;
    ReadOnlyMappedFile& operator=(ReadOnlyMappedFile&&) = delete;

    bool initialize(HANDLE file, std::uint64_t size) {
        reset();

        fileSize = size;

        SYSTEM_INFO systemInfo = {};
        GetSystemInfo(&systemInfo);
        allocationGranularity = systemInfo.dwAllocationGranularity;

        mappingHandle = CreateFileMapping(file, nullptr, PAGE_READONLY, 0, 0, nullptr);
        if (!mappingHandle) {
            lastErrorCode = GetLastError();
            return false;
        }

        lastErrorCode = ERROR_SUCCESS;
        return true;
    }

    void reset() {
        prefixView.reset();
        slidingView.reset();

        if (mappingHandle) {
            CloseHandle(mappingHandle);
            mappingHandle = nullptr;
        }

        fileSize = 0;
        allocationGranularity = 0;
        persistentPrefixBytes = 0;
        lastErrorCode = ERROR_SUCCESS;
    }

    bool mapPersistentPrefix(std::uint64_t prefixBytes) {
        persistentPrefixBytes = prefixBytes;
        prefixView.reset();

        if (prefixBytes == 0) {
            return true;
        }

        if (prefixBytes > fileSize || prefixBytes > static_cast<std::uint64_t>(std::numeric_limits<std::size_t>::max())) {
            lastErrorCode = ERROR_INVALID_PARAMETER;
            return false;
        }

        return mapView(prefixView, 0, prefixBytes, prefixBytes);
    }

    const std::uint8_t* getPersistentRange(std::uint64_t offset, std::uint64_t bytes) const {
        if (bytes == 0) {
            return prefixView.data;
        }

        if (!contains(prefixView, offset, bytes)) {
            return nullptr;
        }

        return prefixView.data + static_cast<std::size_t>(offset - prefixView.offset);
    }

    template <class Callback>
    bool visitRange(std::uint64_t offset, std::uint64_t bytes, Callback&& callback) {
        if (bytes == 0) {
            return true;
        }

        if (!rangeWithinFile(offset, bytes)) {
            lastErrorCode = ERROR_INVALID_PARAMETER;
            return false;
        }

        if (contains(prefixView, offset, bytes)) {
            return callback(getPersistentRange(offset, bytes), static_cast<std::size_t>(bytes));
        }
        if (contains(slidingView, offset, bytes)) {
            return callback(slidingView.data + static_cast<std::size_t>(offset - slidingView.offset), static_cast<std::size_t>(bytes));
        }

        std::uint64_t currentOffset = offset;
        std::uint64_t remaining = bytes;
        const std::uint64_t safeWindowBytes = std::min<std::uint64_t>(
            defaultWindowBytes,
            static_cast<std::uint64_t>(std::numeric_limits<std::size_t>::max())
        );
        while (remaining != 0) {
            const std::uint64_t preferredChunkBytes = std::min<std::uint64_t>(
                remaining,
                safeWindowBytes
            );
            if (!mapView(slidingView, currentOffset, preferredChunkBytes, safeWindowBytes)) {
                return false;
            }

            const std::size_t intraViewOffset = static_cast<std::size_t>(currentOffset - slidingView.offset);
            const std::size_t available = slidingView.size - intraViewOffset;
            const std::size_t chunkBytes = static_cast<std::size_t>(std::min<std::uint64_t>(remaining, available));
            if (chunkBytes == 0) {
                lastErrorCode = ERROR_INVALID_PARAMETER;
                return false;
            }

            if (!callback(slidingView.data + intraViewOffset, chunkBytes)) {
                return false;
            }

            currentOffset += chunkBytes;
            remaining -= chunkBytes;
        }

        return true;
    }

    bool copyRange(std::uint64_t offset, std::uint64_t bytes, void* destination) {
        auto* output = static_cast<std::uint8_t*>(destination);
        return visitRange(offset, bytes, [&](const std::uint8_t* chunk, std::size_t chunkBytes) {
            std::memcpy(output, chunk, chunkBytes);
            output += chunkBytes;
            return true;
        });
    }

    std::uint64_t size() const {
        return fileSize;
    }

    std::uint64_t windowBytes() const {
        return defaultWindowBytes;
    }

    std::uint64_t prefixBytes() const {
        return persistentPrefixBytes;
    }

    void releaseSlidingView() {
        slidingView.reset();
    }

    DWORD lastError() const {
        return lastErrorCode;
    }

private:
    struct View {
        const std::uint8_t* data = nullptr;
        std::uint64_t offset = 0;
        std::size_t size = 0;

        void reset() {
            if (data) {
                UnmapViewOfFile(data);
                data = nullptr;
            }

            offset = 0;
            size = 0;
        }
    };

    bool mapView(View& view, std::uint64_t offset, std::uint64_t requiredBytes, std::uint64_t desiredBytes) {
        if (requiredBytes == 0) {
            return true;
        }

        if (contains(view, offset, requiredBytes)) {
            return true;
        }

        if (!rangeWithinFile(offset, requiredBytes) || !mappingHandle || allocationGranularity == 0) {
            lastErrorCode = ERROR_INVALID_PARAMETER;
            return false;
        }

        const std::uint64_t alignedOffset = (offset / allocationGranularity) * allocationGranularity;
        const std::uint64_t intraViewOffset = offset - alignedOffset;

        std::uint64_t mappedBytes = 0;
        if (!tryAdd(intraViewOffset, requiredBytes, mappedBytes)) {
            lastErrorCode = ERROR_INVALID_PARAMETER;
            return false;
        }

        if (desiredBytes > mappedBytes) {
            mappedBytes = desiredBytes;
        }

        const std::uint64_t maxBytes = fileSize - alignedOffset;
        mappedBytes = std::min(mappedBytes, maxBytes);
        if (mappedBytes < intraViewOffset + requiredBytes || mappedBytes > static_cast<std::uint64_t>(std::numeric_limits<std::size_t>::max())) {
            lastErrorCode = ERROR_INVALID_PARAMETER;
            return false;
        }

        view.reset();
        view.data = static_cast<const std::uint8_t*>(
            MapViewOfFile(
                mappingHandle,
                FILE_MAP_READ,
                static_cast<DWORD>(alignedOffset >> 32),
                static_cast<DWORD>(alignedOffset & 0xFFFFFFFFull),
                static_cast<SIZE_T>(mappedBytes)
            )
        );
        if (!view.data) {
            lastErrorCode = GetLastError();
            return false;
        }

        view.offset = alignedOffset;
        view.size = static_cast<std::size_t>(mappedBytes);
        lastErrorCode = ERROR_SUCCESS;
        return true;
    }

    static bool tryAdd(std::uint64_t a, std::uint64_t b, std::uint64_t& result) {
        if (a > std::numeric_limits<std::uint64_t>::max() - b) {
            return false;
        }

        result = a + b;
        return true;
    }

    bool rangeWithinFile(std::uint64_t offset, std::uint64_t bytes) const {
        if (offset > fileSize) {
            return false;
        }
        if (bytes > fileSize - offset) {
            return false;
        }
        return true;
    }

    static bool contains(const View& view, std::uint64_t offset, std::uint64_t bytes) {
        if (bytes == 0) {
            return true;
        }
        if (!view.data || offset < view.offset) {
            return false;
        }

        const std::uint64_t relativeOffset = offset - view.offset;
        if (relativeOffset > static_cast<std::uint64_t>(view.size)) {
            return false;
        }

        const std::uint64_t available = static_cast<std::uint64_t>(view.size) - relativeOffset;
        return bytes <= available;
    }

    HANDLE mappingHandle = nullptr;
    std::uint64_t fileSize = 0;
    std::uint64_t defaultWindowBytes = 0;
    DWORD allocationGranularity = 0;
    std::uint64_t persistentPrefixBytes = 0;
    DWORD lastErrorCode = ERROR_SUCCESS;
    View prefixView;
    View slidingView;
};

namespace MappedFileUtil {
    inline bool QueryFileSize(HANDLE file, std::uint64_t& fileSize, const char* winErrorMessage) {
        LARGE_INTEGER size = {};
        if (!GetFileSizeEx(file, &size)) {
            LOG::winerror("%s", winErrorMessage);
            return false;
        }

        fileSize = static_cast<std::uint64_t>(size.QuadPart);
        return true;
    }

    inline bool LogMappingFailure(const char* winErrorMessage, const ReadOnlyMappedFile& mapping) {
        const DWORD error = mapping.lastError();
        if (error != ERROR_SUCCESS && error != ERROR_INVALID_PARAMETER) {
            SetLastError(error);
            LOG::winerror("%s", winErrorMessage);
        } else {
            LOG::logline("!! %s", winErrorMessage);
        }
        return false;
    }
}
