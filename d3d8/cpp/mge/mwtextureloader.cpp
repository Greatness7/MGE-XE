#include "mge/mwtextureloader.h"
#include "mge/mwpatches.h"
#include "mge/bc7format.h"
#include "support/log.h"

#include <cstddef>
#include <cstring>


namespace MWTextureLoader {

using MWPatches::VirtualMemWriteAccessor;
using MWPatches::write_byte;
using MWPatches::write_ptr;

static bool bc7TextureSupported = false;

void setBC7TextureSupport(bool supported) {
    bc7TextureSupported = supported;
}

//-----------------------------------------------------------------------------

struct NiDX8Renderer {
    void* vtbl;
    int unknown_0x4[8];
    IDirect3DDevice8* d3dDevice;
    int unknown_0x28[414];
};
static_assert(sizeof(NiDX8Renderer) == 0x6A0);

struct NiPixelFormat {
    int format;
    unsigned int channelMasks[4];
    unsigned int bitsPerPixel;
    unsigned int compareBits[2];
};
static_assert(sizeof(NiPixelFormat) == 0x20);

struct NiPixelData {
    void* vtbl;
    unsigned int refCount;
    NiPixelFormat format;
    void* palette;
    unsigned char* pixelsAllMips;
    unsigned int* mipmapWidths;
    unsigned int* mipmapHeights;
    unsigned int* mipmapOffsets;
    unsigned int mipmapLevels;
    unsigned int bytesPerPixel;
    unsigned int revisionID;
};
static_assert(sizeof(NiPixelData) == 0x48);
static_assert(offsetof(NiPixelData, format) == 0x08);
static_assert(offsetof(NiPixelData, mipmapLevels) == 0x3C);

struct NiSourceTexture {
    void* vtbl;
    int unknown_0x4[10];
    const char* filename;
    const char* filenameOnPC;
    NiPixelData* pixelData;
    bool isStatic;
};
static_assert(sizeof(NiSourceTexture) == 0x3C);
static_assert(offsetof(NiSourceTexture, pixelData) == 0x34);

struct NiDX8RendererTextureData {
    void* vtbl;
    void* unknown_0x4;
    NiSourceTexture* sourceTexture;
    void* unknown_0xC;
    NiDX8Renderer* renderer;
    int pixelFormat[10];
    void* d3dPalette;
    int d3dPaletteRevision;
    unsigned int width, height;
    unsigned int levels;
    bool bMipmap;
    int unknown_0x54;
    void* sourcePalette;
    int sourcePaletteRevision;
    IDirect3DTexture8* d3dTexture;
    int sourceRevision;
};
static_assert(sizeof(NiDX8RendererTextureData) == 0x68);

struct NiFile;

struct NiFileVtbl {
    void* deletingDtor;
    void* asBool;
    unsigned int (__thiscall* read)(NiFile*, void*, unsigned int);
    void* write;
    void (__thiscall* seek)(NiFile*, int, int);
};

struct NiFile {
    NiFileVtbl* vtbl;
    void* buffer;
    unsigned int bufferAllocSize;
    unsigned int bufferReadSize;
    unsigned int position;
    void* filePointer;
    int accessMode;
    bool valid;
};
static_assert(sizeof(NiFile) == 0x20);
static_assert(offsetof(NiFile, position) == 0x10);

struct NiDDSReader {
    void* vtbl;
    unsigned int width;
    unsigned int height;
    unsigned int mipMapLevels;
    NiPixelFormat pixelFormat;
};
static_assert(sizeof(NiDDSReader) == 0x30);
static_assert(offsetof(NiDDSReader, pixelFormat) == 0x10);

struct DDSHeaderDX10 {
    unsigned int dxgiFormat;
    unsigned int resourceDimension;
    unsigned int miscFlag;
    unsigned int arraySize;
    unsigned int miscFlags2;
};
static_assert(sizeof(DDSHeaderDX10) == 0x14);

using NiDDSReaderReadFile = NiPixelData* (__thiscall*)(NiDDSReader*, NiFile*, NiPixelData*);
using NiDDSReaderDecodeHeader = bool (__thiscall*)(NiDDSReader*, NiFile*, unsigned int*, unsigned int*, NiPixelFormat*, bool*);

static const auto niDDSReaderReadFile = reinterpret_cast<NiDDSReaderReadFile>(0x708050);
static const auto niDDSReaderDecodeHeader = reinterpret_cast<NiDDSReaderDecodeHeader>(0x707C10);

static bool readExact(NiFile* file, void* data, unsigned int length) {
    return file->vtbl->read(file, data, length) == length;
}

static bool __fastcall patchNiDDSReaderDecodeHeader(
    NiDDSReader* reader,
    void*,
    NiFile* file,
    unsigned int* outWidth,
    unsigned int* outHeight,
    NiPixelFormat* outPixelFormat,
    bool* outMipmap)
{
    const unsigned int startPosition = file->position;
    if (niDDSReaderDecodeHeader(reader, file, outWidth, outHeight, outPixelFormat, outMipmap)) {
        // Compressed channel masks are unused, so reserve the first one exclusively for our tag.
        if (reader->pixelFormat.format >= 6 && reader->pixelFormat.format <= 8) {
            reader->pixelFormat.channelMasks[0] = 0;
            outPixelFormat->channelMasks[0] = 0;
        }
        return true;
    }

    // An unknown DDS FourCC is the only original failure at this exact cursor position.
    if (file->position != startPosition + 88) {
        return false;
    }

    constexpr unsigned int ddsFourCCDX10 = MAKEFOURCC('D', 'X', '1', '0');
    constexpr unsigned int dxgiFormatBC7Unorm = 98;
    constexpr unsigned int dxgiFormatBC7UnormSrgb = 99;
    constexpr unsigned int d3d10ResourceDimensionTexture2D = 3;
    constexpr unsigned int d3d11ResourceMiscTextureCube = 0x4;
    constexpr unsigned int ddsCapsComplex = 0x8;
    constexpr unsigned int ddsCapsMipmap = 0x400000;
    constexpr int seekCurrent = 1;

    unsigned int fourCC;
    file->vtbl->seek(file, -4, seekCurrent);
    if (!readExact(file, &fourCC, sizeof(fourCC)) || fourCC != ddsFourCCDX10) {
        return false;
    }

    unsigned int mipMapCount;
    file->vtbl->seek(file, -60, seekCurrent);
    if (!readExact(file, &mipMapCount, sizeof(mipMapCount))) {
        return false;
    }

    unsigned int caps;
    unsigned int caps2;
    file->vtbl->seek(file, 76, seekCurrent);
    if (!readExact(file, &caps, sizeof(caps))
        || (caps & 0x102) != 0
        || !readExact(file, &caps2, sizeof(caps2))
        || (caps2 & 0x200) != 0)
    {
        return false;
    }

    DDSHeaderDX10 dx10;
    file->vtbl->seek(file, 12, seekCurrent);
    if (!readExact(file, &dx10, sizeof(dx10))
        || (dx10.dxgiFormat != dxgiFormatBC7Unorm && dx10.dxgiFormat != dxgiFormatBC7UnormSrgb)
        || dx10.resourceDimension != d3d10ResourceDimensionTexture2D
        || dx10.arraySize != 1
        || (dx10.miscFlag & d3d11ResourceMiscTextureCube) != 0)
    {
        return false;
    }

    if (!bc7TextureSupported) {
        LOG::logline("BC7 DDS texture rejected: the active D3D9 runtime does not support MGE XE's BC7 format.");
        return false;
    }

    const auto niPixelFormatCtor = reinterpret_cast<void (__thiscall*)(NiPixelFormat*, int)>(0x6EDA40);
    niPixelFormatCtor(&reader->pixelFormat, 8);
    reader->pixelFormat.channelMasks[0] = static_cast<unsigned int>(MGE_D3DFMT_BC7);

    *outWidth = reader->height;
    *outHeight = reader->width;
    *outPixelFormat = reader->pixelFormat;

    if ((caps & ddsCapsComplex) != 0 && (caps & ddsCapsMipmap) != 0 && mipMapCount != 1) {
        *outMipmap = true;
        reader->mipMapLevels = mipMapCount;
    } else {
        *outMipmap = false;
        reader->mipMapLevels = 1;
    }
    return true;
}

static NiPixelData* __fastcall patchNiDDSReaderReadFile(
    NiDDSReader* reader,
    void*,
    NiFile* file,
    NiPixelData* pixelData)
{
    NiPixelData* result = niDDSReaderReadFile(reader, file, pixelData);
    if (result && reader->pixelFormat.format >= 6 && reader->pixelFormat.format <= 8) {
        // ReadFile can reuse PixelData when only this tag differs, so refresh it after every read.
        result->format.channelMasks[0] = reader->pixelFormat.channelMasks[0];
    }
    return result;
}

static HRESULT __stdcall patchLoadTexture2DCreate(
    IDirect3DDevice8* device,
    NiDX8RendererTextureData* sourceTextureData,
    const NiPixelData* pixelData,
    D3DFORMAT d3dFormat) {
    // Static texture: Create staging texture in system memory pool
    // Dynamic texture: Create texture in managed pool
    auto width = sourceTextureData->width, height = sourceTextureData->height, levels = sourceTextureData->levels;
    auto pool = sourceTextureData->sourceTexture->isStatic ? D3DPOOL_SYSTEMMEM : D3DPOOL_MANAGED;

    if (pixelData && pixelData->format.format == 8
        && pixelData->format.channelMasks[0] == static_cast<unsigned int>(MGE_D3DFMT_BC7))
    {
        d3dFormat = MGE_D3DFMT_BC7;
    }

    void* d3d8Vtbl = *reinterpret_cast<void**>(device);
    auto d3d8CreateTexture = *reinterpret_cast<HRESULT(__stdcall**)(IDirect3DDevice8*, UINT, UINT, UINT, DWORD, DWORD, DWORD, IDirect3DTexture8**)>((char*)d3d8Vtbl + 0x50);

    return d3d8CreateTexture(device, width, height, levels, 0, d3dFormat, pool, &sourceTextureData->d3dTexture);
}

static void __stdcall patchLoadTexture2DUpload(
    NiDX8RendererTextureData* sourceTextureData,
    const NiPixelData* pixelData,
    D3DFORMAT d3dFormat) {
    // This upload step is only needed if it is a static texture
    if (sourceTextureData->sourceTexture->isStatic) {
        auto width = sourceTextureData->width, height = sourceTextureData->height, levels = sourceTextureData->levels;
        IDirect3DDevice8* device = sourceTextureData->renderer->d3dDevice;
        IDirect3DTexture8* stagingTexture = sourceTextureData->d3dTexture;
        IDirect3DTexture8* texture = nullptr;

        // The staging texture creation can fail, for example on a block-compressed texture whose
        // dimensions are not a multiple of the 4x4 block size. Morrowind checks that result, clears
        // d3dTexture and carries on, but it still falls through to this patch site. There is nothing
        // to promote to the default pool in that case, and the default pool creation would fail for
        // the same reason anyway.
        if (!stagingTexture) {
            return;
        }

        if (pixelData && pixelData->format.format == 8
            && pixelData->format.channelMasks[0] == static_cast<unsigned int>(MGE_D3DFMT_BC7))
        {
            d3dFormat = MGE_D3DFMT_BC7;
        }

        // Create texture in default pool
        void* d3d8Vtbl = *reinterpret_cast<void**>(device);
        auto d3d8CreateTexture = *reinterpret_cast<HRESULT(__stdcall**)(IDirect3DDevice8*, UINT, UINT, UINT, DWORD, DWORD, DWORD, IDirect3DTexture8**)>((char*)d3d8Vtbl + 0x50);
        auto d3d8UpdateTexture = *reinterpret_cast<HRESULT(__stdcall**)(IDirect3DDevice8*, IDirect3DTexture8*, IDirect3DTexture8*)>((char*)d3d8Vtbl + 0x74);

        if (FAILED(d3d8CreateTexture(device, width, height, levels, 0, d3dFormat, D3DPOOL_DEFAULT, &texture))) {
            sourceTextureData->d3dTexture = nullptr;
            reinterpret_cast<IUnknown*>(stagingTexture)->Release();
            return;
        }

        // Move texture from staging into final texture
        d3d8UpdateTexture(device, stagingTexture, texture);
        sourceTextureData->d3dTexture = texture;
        reinterpret_cast<IUnknown*>(stagingTexture)->Release();
    }
}

void patchLoadTexture2D() {
    DWORD addr1 = 0x6BFC4B, addr2 = 0x6BFD3B, addr3 = 0x6BFCC1;
    BYTE patch1[] = {
        0x52,                               // push edx (d3dFormat)
        0x55,                               // push ebp (convertedPixelData)
        0x56,                               // push esi (sourceTextureData)
        0x53,                               // push ebx (d3dDevice)
        0xb8, 0xff, 0xff, 0xff, 0xff,       // mov eax, newfunc
        0xff, 0xd0,                         // call eax
        0xeb, 0x0e                          // jmp past rest of block
    };
    BYTE patch2[] = {
        0x8b, 0x54, 0x24, 0x10,             // mov edx, [esp+d3dFormat]
        0x52,                               // push edx (d3dFormat)
        0x55,                               // push ebp (convertedPixelData)
        0x56,                               // push esi (sourceTextureData)
        0xb8, 0xff, 0xff, 0xff, 0xff,       // mov eax, newfunc
        0xff, 0xd0,                         // call eax
        0xeb, 0x08                          // jmp past rest of block
    };

    // Initially load texture into a staging texture if static
    VirtualMemWriteAccessor vw1((void*)addr1, sizeof(patch1));
    memcpy((void*)addr1, patch1, sizeof(patch1));
    write_ptr(addr1 + 5, reinterpret_cast<void*>(patchLoadTexture2DCreate));

    // Overwrite some useless code path with a call to upload the staging texture to a default pool texture
    VirtualMemWriteAccessor vw2((void*)addr2, sizeof(patch2));
    memcpy((void*)addr2, patch2, sizeof(patch2));
    write_ptr(addr2 + 8, reinterpret_cast<void*>(patchLoadTexture2DUpload));

    // Make this code re-use another dead stack variable instead of the d3dFormat variable which is still needed
    VirtualMemWriteAccessor vw3((void*)addr3, 0x40);
    write_byte(0x6BFCC7, 0x18);
    write_byte(0x6BFCD7, 0x18);
    write_byte(0x6BFCE3, 0x24);

    // NiDDSReader is reached only through these two adjacent .rdata vtable slots.
    constexpr DWORD readFileSlot = 0x751328;
    constexpr DWORD decodeHeaderSlot = 0x75132C;
    VirtualMemWriteAccessor vw4(reinterpret_cast<void*>(readFileSlot), 2 * sizeof(void*), PAGE_READWRITE);
    write_ptr(readFileSlot, reinterpret_cast<void*>(patchNiDDSReaderReadFile));
    write_ptr(decodeHeaderSlot, reinterpret_cast<void*>(patchNiDDSReaderDecodeHeader));
}

} // namespace MWTextureLoader
