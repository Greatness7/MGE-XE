
#include "morrowindbsa.h"
#include "bc7format.h"

#include <cstdio>
#include <cstdint>
#include <cstring>
#include <unordered_map>
#include <memory>



namespace BSA {

using std::unordered_map;

struct CacheEntry {
    HANDLE file;
    DWORD position;
    DWORD size;
};

struct BSAHash3 {
    union {
        struct {
            DWORD value1, value2;
        };
        __int64 LValue;
    };
};

struct EntryData {
    std::unique_ptr<char[]> data;
    unsigned int size;

    bool valid() const { return bool(data); }
};

struct BC7DDSInfo {
    unsigned int width;
    unsigned int height;
    unsigned int mipLevels;
};

constexpr size_t bc7DDSDataOffset = 148;

static unsigned int readU32(const unsigned char* data) {
    unsigned int value;
    std::memcpy(&value, data, sizeof(value));
    return value;
}

static bool parseBC7DDS(const void* fileData, size_t fileSize, BC7DDSInfo* info) {
    constexpr unsigned int ddsMagic = MAKEFOURCC('D', 'D', 'S', ' ');
    constexpr unsigned int ddsFourCCDX10 = MAKEFOURCC('D', 'X', '1', '0');
    constexpr unsigned int ddpfFourCC = 0x4;
    constexpr unsigned int dxgiFormatBC7Unorm = 98;
    constexpr unsigned int dxgiFormatBC7UnormSrgb = 99;
    constexpr unsigned int d3d10ResourceDimensionTexture2D = 3;
    constexpr unsigned int d3d11ResourceMiscTextureCube = 0x4;

    if (fileSize < bc7DDSDataOffset) {
        return false;
    }

    const auto* data = static_cast<const unsigned char*>(fileData);
    const unsigned int width = readU32(data + 16);
    const unsigned int height = readU32(data + 12);
    unsigned int mipLevels = readU32(data + 28);
    const unsigned int dxgiFormat = readU32(data + 128);

    if (readU32(data) != ddsMagic
        || readU32(data + 4) != 124
        || readU32(data + 76) != 32
        || (readU32(data + 80) & ddpfFourCC) == 0
        || readU32(data + 84) != ddsFourCCDX10
        || (dxgiFormat != dxgiFormatBC7Unorm && dxgiFormat != dxgiFormatBC7UnormSrgb)
        || readU32(data + 132) != d3d10ResourceDimensionTexture2D
        || (readU32(data + 136) & d3d11ResourceMiscTextureCube) != 0
        || readU32(data + 140) != 1
        || width == 0
        || height == 0)
    {
        return false;
    }

    if (mipLevels == 0) {
        mipLevels = 1;
    }

    unsigned int maxMipLevels = 1;
    for (unsigned int mipWidth = width, mipHeight = height;
         mipWidth > 1 || mipHeight > 1;
         mipWidth = mipWidth > 1 ? mipWidth >> 1 : 1,
         mipHeight = mipHeight > 1 ? mipHeight >> 1 : 1)
    {
        ++maxMipLevels;
    }
    if (mipLevels > maxMipLevels) {
        return false;
    }

    std::uint64_t requiredSize = bc7DDSDataOffset;
    unsigned int mipWidth = width;
    unsigned int mipHeight = height;
    for (unsigned int level = 0; level < mipLevels; ++level) {
        const std::uint64_t blocksWide = (static_cast<std::uint64_t>(mipWidth) + 3) / 4;
        const std::uint64_t blocksHigh = (static_cast<std::uint64_t>(mipHeight) + 3) / 4;
        requiredSize += blocksWide * blocksHigh * 16;
        mipWidth = mipWidth > 1 ? mipWidth >> 1 : 1;
        mipHeight = mipHeight > 1 ? mipHeight >> 1 : 1;
    }
    if (requiredSize > fileSize) {
        return false;
    }

    if (info) {
        info->width = width;
        info->height = height;
        info->mipLevels = mipLevels;
    }
    return true;
}

// D3DX9 cannot parse DX10 DDS headers, so copy BC7 blocks through a staging texture directly.
static IDirect3DTexture9* loadBC7TextureFromMemory(IDirect3DDevice9* dev, const void* fileData, size_t fileSize) {
    BC7DDSInfo info;
    if (!parseBC7DDS(fileData, fileSize, &info)) {
        return nullptr;
    }

    IDirect3DTexture9* stagingTexture = nullptr;
    if (FAILED(dev->CreateTexture(info.width, info.height, info.mipLevels, 0, MGE_D3DFMT_BC7,
                                  D3DPOOL_SYSTEMMEM, &stagingTexture, nullptr))) {
        return nullptr;
    }

    const auto* source = static_cast<const unsigned char*>(fileData) + bc7DDSDataOffset;
    unsigned int mipWidth = info.width;
    unsigned int mipHeight = info.height;
    for (unsigned int level = 0; level < info.mipLevels; ++level) {
        const size_t rowBytes = ((static_cast<size_t>(mipWidth) + 3) / 4) * 16;
        const size_t blockRows = (static_cast<size_t>(mipHeight) + 3) / 4;
        D3DLOCKED_RECT lockedRect;
        if (FAILED(stagingTexture->LockRect(level, &lockedRect, nullptr, 0))) {
            stagingTexture->Release();
            return nullptr;
        }

        if (lockedRect.Pitch < 0 || static_cast<size_t>(lockedRect.Pitch) < rowBytes) {
            stagingTexture->UnlockRect(level);
            stagingTexture->Release();
            return nullptr;
        }

        auto* destination = static_cast<unsigned char*>(lockedRect.pBits);
        for (size_t row = 0; row < blockRows; ++row) {
            std::memcpy(destination + row * lockedRect.Pitch, source + row * rowBytes, rowBytes);
        }
        stagingTexture->UnlockRect(level);

        source += rowBytes * blockRows;
        mipWidth = mipWidth > 1 ? mipWidth >> 1 : 1;
        mipHeight = mipHeight > 1 ? mipHeight >> 1 : 1;
    }

    IDirect3DTexture9* texture = nullptr;
    if (FAILED(dev->CreateTexture(info.width, info.height, info.mipLevels, 0, MGE_D3DFMT_BC7,
                                  D3DPOOL_DEFAULT, &texture, nullptr))
        || FAILED(dev->UpdateTexture(stagingTexture, texture)))
    {
        if (texture) {
            texture->Release();
        }
        stagingTexture->Release();
        return nullptr;
    }

    stagingTexture->Release();
    return texture;
}

static IDirect3DTexture9* loadBC7TextureFromFile(IDirect3DDevice9* dev, const char* path) {
    HANDLE file = CreateFile(path, GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING, 0, nullptr);
    if (file == INVALID_HANDLE_VALUE) {
        return nullptr;
    }

    LARGE_INTEGER fileSize;
    if (!GetFileSizeEx(file, &fileSize) || fileSize.QuadPart < bc7DDSDataOffset || fileSize.HighPart != 0) {
        CloseHandle(file);
        return nullptr;
    }

    const unsigned int size = static_cast<unsigned int>(fileSize.QuadPart);
    unsigned char header[bc7DDSDataOffset];
    DWORD bytesRead;
    if (!ReadFile(file, header, sizeof(header), &bytesRead, nullptr)
        || bytesRead != sizeof(header)
        || !parseBC7DDS(header, size, nullptr))
    {
        CloseHandle(file);
        return nullptr;
    }

    auto data = std::make_unique<char[]>(size);
    std::memcpy(data.get(), header, sizeof(header));
    const unsigned int remainingSize = size - sizeof(header);
    const bool read = ReadFile(file, data.get() + sizeof(header), remainingSize, &bytesRead, nullptr)
                      && bytesRead == remainingSize;
    CloseHandle(file);
    if (!read) {
        return nullptr;
    }

    return loadBC7TextureFromMemory(dev, data.get(), size);
}

// Note: loadedTextures would ideally store weakRefs, but COM doesn't support those.
static unordered_map<__int64, CacheEntry> cacheMap;
static unordered_map<__int64, IDirect3DTexture9*> loadedTextures;



// Use GhostWheel's code to hash the string
static BSAHash3 hashString(const char* str) {
    BSAHash3 result;

    unsigned int len = (unsigned int)strlen(str);

    unsigned int l = len >> 1;
    unsigned int sum, off, temp, i, n;

    for (sum = off = i = 0; i < l; i++) {
        sum ^= ((unsigned int)(str[i])) << (off & 0x1F);
        off += 8;
    }
    result.value1 = sum;

    for (sum = off = 0; i < len; i++) {
        temp = ((unsigned int)(str[i])) << (off & 0x1F);
        sum ^= temp;
        n = temp & 0x1F;
        sum = (sum << (32-n)) | (sum >> n);  // binary rotate right
        off += 8;
    }
    result.value2 = sum;
    return result;
}

static void open(const char* path) {
    HANDLE bsa = CreateFile(path, GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0);
    if (bsa == INVALID_HANDLE_VALUE) {
        return;
    }

    DWORD hashOffset, numFiles, bytesRead, unused;
    ReadFile(bsa, &hashOffset, 4, &bytesRead, 0);
    if (bytesRead != 4 || hashOffset != 0x100) {
        CloseHandle(bsa);
        return;
    }

    ReadFile(bsa, &hashOffset, 4, &unused, 0);
    ReadFile(bsa, &numFiles, 4, &unused, 0);
    for (DWORD i = 0; i < numFiles; i++) {
        CacheEntry entry;
        __int64 hash;

        entry.file = bsa;
        SetFilePointer(bsa, 12 + i*8, 0, FILE_BEGIN);
        ReadFile(bsa, &entry.size, 4, &unused, 0);
        ReadFile(bsa, &entry.position, 4, &unused, 0);
        entry.position += 12 + hashOffset + numFiles*8;

        SetFilePointer(bsa, 12 + hashOffset + i*8, 0, FILE_BEGIN);
        ReadFile(bsa, &hash, 8, &unused, 0);
        cacheMap[hash] = entry;
    }
}

void init() {
    char path[MAX_PATH];
    WIN32_FIND_DATA data;

    HANDLE h = FindFirstFile("Data Files\\*.bsa", &data);
    if (h == INVALID_HANDLE_VALUE) {
        return;
    }

    do {
        std::snprintf(path, sizeof(path), "Data Files\\%s", data.cFileName);
        open(path);
    } while (FindNextFile(h, &data));

    FindClose(h);
}

static EntryData BSALoadFile(BSAHash3 hash) {
    auto it = cacheMap.find(hash.LValue);
    if (it == cacheMap.end()) {
        return EntryData();
    }

    const CacheEntry& entry = it->second;
    auto buf = std::make_unique<char[]>(entry.size);
    DWORD bytesRead;

    SetFilePointer(entry.file, entry.position, 0, FILE_BEGIN);
    ReadFile(entry.file, buf.get(), entry.size, &bytesRead, 0);

    if (bytesRead == entry.size) {
        return EntryData { std::move(buf), entry.size };
    } else {
        return EntryData();
    }
}

static IDirect3DTexture9* loadTextureExact(IDirect3DDevice9* dev, const char* filename) {
    char pathbuf[MAX_PATH];
    BSAHash3 hash = hashString(filename);
    IDirect3DTexture9* tex = nullptr;

    // First check if the texture is already loaded. A cached entry may be null, as failed loads
    // are cached too; pass that through so callers can fall back to another extension.
    auto it = loadedTextures.find(hash.LValue);
    if (it != loadedTextures.end()) {
        if (it->second) {
            it->second->AddRef();
        }
        return it->second;
    }

    // Generated distant-land files take precedence over loose Data Files, which take precedence
    // over BSA archives.
    std::snprintf(pathbuf, sizeof(pathbuf), "Data Files\\distantland\\statics\\%s", filename);
    if (GetFileAttributes(pathbuf) != INVALID_FILE_ATTRIBUTES) {
        tex = loadBC7TextureFromFile(dev, pathbuf);
        HRESULT hr = tex ? D3D_OK
                         : D3DXCreateTextureFromFileEx(dev, pathbuf, D3DX_FROM_FILE, D3DX_FROM_FILE, D3DX_FROM_FILE, 0,
                                                      D3DFMT_UNKNOWN, D3DPOOL_DEFAULT, D3DX_FILTER_NONE,
                                                      D3DX_FILTER_NONE, 0, 0, 0, &tex);

        if (hr == D3D_OK) {
            // Cache failures too, so a missing or undecodable asset is not retried every draw.
            loadedTextures[hash.LValue] = tex;
            return tex;
        }
    }

    std::snprintf(pathbuf, sizeof(pathbuf), "Data Files\\%s", filename);
    if (GetFileAttributes(pathbuf) != INVALID_FILE_ATTRIBUTES) {
        tex = loadBC7TextureFromFile(dev, pathbuf);
        HRESULT hr = tex ? D3D_OK
                         : D3DXCreateTextureFromFileEx(dev, pathbuf, D3DX_FROM_FILE, D3DX_FROM_FILE, D3DX_FROM_FILE, 0,
                                                      D3DFMT_UNKNOWN, D3DPOOL_DEFAULT, D3DX_FILTER_NONE,
                                                      D3DX_FILTER_NONE, 0, 0, 0, &tex);

        if (hr == D3D_OK) {
            loadedTextures[hash.LValue] = tex;
            return tex;
        }
    }

    EntryData ed = BSALoadFile(hash);
    if (ed.valid()) {
        tex = loadBC7TextureFromMemory(dev, ed.data.get(), ed.size);

        // Use D3DPOOL_DEFAULT to match the loose-file paths above; D3DPOOL_MANAGED would keep a
        // system-memory copy of every BSA-sourced distant static texture in the 32-bit process.
        if (!tex) {
            D3DXCreateTextureFromFileInMemoryEx(dev, ed.data.get(), ed.size, D3DX_FROM_FILE, D3DX_FROM_FILE,
                                                D3DX_FROM_FILE, 0, D3DFMT_UNKNOWN, D3DPOOL_DEFAULT, D3DX_DEFAULT,
                                                D3DX_DEFAULT, 0, 0, 0, &tex);
        }

        loadedTextures[hash.LValue] = tex;
        return tex;
    }

    return nullptr;
}

IDirect3DTexture9* loadTexture(IDirect3DDevice9* dev, const char* filename) {
    char pathbuf[MAX_PATH];

    // Prefer the DDS extension before loading the original texture format.
    std::snprintf(pathbuf, sizeof(pathbuf), "textures\\%s", filename);
    std::strcpy(pathbuf + strlen(pathbuf) - 3, "dds");

    IDirect3DTexture9* tex = loadTextureExact(dev, pathbuf);
    if (tex) {
        return tex;
    }

    std::snprintf(pathbuf, sizeof(pathbuf), "textures\\%s", filename);
    return loadTextureExact(dev, pathbuf);
}

void clearTextureCache() {
    for (auto& entry : loadedTextures) {
        if (entry.second) {
            entry.second->Release();
        }
    }
    loadedTextures.clear();
}

}
