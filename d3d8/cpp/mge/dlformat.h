#pragma once

#include "ipc/bridge.h"
#include "dlmath.h"
#include "distantshader.h"
#include <array>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <string>
#include <type_traits>
#include <vector>

enum StaticType {
    STATIC_AUTO = 0,
    STATIC_NEAR = 1,
    STATIC_FAR = 2,
    STATIC_VERY_FAR = 3,
    STATIC_GRASS = 4,
    STATIC_TREE = 5,
    STATIC_BUILDING = 6
};

struct LandMesh {
    BoundingSphere sphere;
    BoundingBox box;
    DWORD verts;
    DWORD faces;
    ptr32<IDirect3DVertexBuffer9> vbuffer;
    ptr32<IDirect3DIndexBuffer9> ibuffer;
};

#pragma pack(push, 4)
struct HorizonFootprint {
    float maxZ;
    std::uint8_t vertexCount;
    std::uint8_t padding[3];
    float footprintXY[6][2];
};

struct DistantSubset {
    BoundingSphere sphere;
    D3DXVECTOR3 aabbMin, aabbMax;       // corners of the axis-aligned bounding box
    ptr32<IDirect3DTexture9> tex;
    bool hasAlpha, hasUVController;
    ptr32<IDirect3DVertexBuffer9> vbuffer;
    ptr32<IDirect3DIndexBuffer9> ibuffer;
    int verts;
    int faces;
    HorizonFootprint horizonFootprint;
    int farFaces;
    int veryFarFaces;
};

struct DistantStatic {
    unsigned char type;
    BoundingSphere sphere;
    D3DXVECTOR3 aabbMin, aabbMax;       // corners of the axis-aligned bounding box
    DWORD firstSubsetIndex;
    DWORD numSubsets;
};
#pragma pack(pop)

struct UsedDistantStatic {
    DWORD staticRef;
    uint16_t visIndex;
    D3DXVECTOR3 pos;
    float scale;
    D3DXMATRIX transform;
    BoundingSphere sphere;      // post-transform
    BoundingBox box;            // post-transform
};

namespace StaticMeshesBin {
    static constexpr std::uint8_t FileMagic[8] = { 'X', 'E', 'S', 'T', 'A', 'T', '0', '6' };
    static constexpr std::uint32_t FileVersion = 6;
    static constexpr std::uint32_t SerializedHeaderSize = 160;
    static constexpr std::uint32_t SerializedStaticRecordSize = 52;
    static constexpr std::uint32_t SerializedSubsetRecordSize = 152;
    static constexpr std::uint32_t SerializedComponentRecordSize = 16;
    static constexpr std::uint32_t SerializedPaletteRecordSize = 16;
    static constexpr std::uint32_t VertexStride = 20;
    static constexpr std::uint32_t GrassVertexStride = 20;
    static constexpr std::uint32_t IndexElementSize = 2;
    static constexpr std::uint32_t KnownFlagsMask = 0x3;

    // Maximum UV-bound palette entries in one subset. Interlock across three languages, all of
    // which must move together: this constant, `UV_BOUND_PALETTE_CAP` in
    // `distantland/crates/formats/src/distant_statics.rs`, and the `uvBoundPalette[N]` array size
    // in `assets/Data Files/shaders/core/XE Common.fx`. Rust and C++ disagreeing fails loudly at
    // load; HLSL disagreeing does not, and shows as wrong atlas tiles on a few subsets.
    static constexpr std::uint32_t MaxPaletteEntries = 128;

    struct Vec3 {
        float x;
        float y;
        float z;

        D3DXVECTOR3 toRuntime() const {
            return D3DXVECTOR3(x, y, z);
        }
    };

    struct BoundingSphere {
        float radius;
        Vec3 center;

        ::BoundingSphere toRuntime() const {
            ::BoundingSphere sphere;
            sphere.center = center.toRuntime();
            sphere.radius = radius;
            return sphere;
        }
    };

    struct Aabb {
        Vec3 min;
        Vec3 max;

        D3DXVECTOR3 minRuntime() const {
            return min.toRuntime();
        }

        D3DXVECTOR3 maxRuntime() const {
            return max.toRuntime();
        }
    };

    using HorizonFootprint = ::HorizonFootprint;

    struct StaticMeshesFileHeader {
        std::uint8_t magic[8];
        std::uint32_t version;
        std::uint32_t header_size;
        std::uint32_t static_record_size;
        std::uint32_t subset_record_size;
        std::uint32_t vertex_stride;
        std::uint32_t index_element_size;
        std::uint32_t static_count;
        std::uint32_t subset_count;
        std::uint64_t static_table_offset;
        std::uint64_t static_table_size;
        std::uint64_t subset_table_offset;
        std::uint64_t subset_table_size;
        std::uint64_t texture_blob_offset;
        std::uint64_t texture_blob_size;
        std::uint64_t geometry_blob_offset;
        std::uint64_t geometry_blob_size;
        std::uint32_t grass_vertex_stride;
        std::uint32_t reserved;
        std::uint64_t component_table_offset;
        std::uint64_t component_table_size;
        std::uint32_t component_record_size;
        std::uint32_t component_count;
        std::uint64_t palette_table_offset;
        std::uint64_t palette_table_size;
        std::uint32_t palette_record_size;
        std::uint32_t palette_count;
    };

    struct StaticRecord {
        std::uint32_t static_type;
        BoundingSphere sphere;
        Aabb aabb;
        std::uint32_t first_subset_index;
        std::uint32_t subset_count;
    };

    struct SubsetRecord {
        BoundingSphere sphere;
        Aabb aabb;
        std::uint64_t texture_path_offset;
        std::uint64_t vertex_offset;
        std::uint64_t index_offset;
        std::uint32_t vertex_count;
        std::uint32_t triangle_count;
        std::uint32_t flags;
        std::uint32_t texture_path_length;
        HorizonFootprint horizonFootprint;
        std::uint32_t first_component_index;
        std::uint32_t component_count;
        std::uint32_t first_palette_index;
        std::uint32_t palette_count;
    };

    struct ComponentRecord {
        std::uint32_t first_triangle;
        std::uint32_t triangle_count;
        float radius;
        std::uint8_t classification;
        std::uint8_t reserved[3];
    };

    // One atlas rect in a subset's UV-bound palette, lane order [min_v, max_u, min_u, max_v].
    // Static vertices select an entry by the ordinal in position.w.
    struct PaletteRecord {
        float bound[4];
    };

    static_assert(sizeof(HorizonFootprint) == 56, "Static mesh horizon footprint ABI drifted");
    static_assert(alignof(HorizonFootprint) == 4, "Static mesh horizon footprint alignment drifted");
    static_assert(std::is_standard_layout<HorizonFootprint>::value, "Static mesh horizon footprint must stay POD");
    static_assert(offsetof(HorizonFootprint, maxZ) == 0, "Static mesh horizon footprint maxZ offset drifted");
    static_assert(offsetof(HorizonFootprint, vertexCount) == 4, "Static mesh horizon footprint vertexCount offset drifted");
    static_assert(offsetof(HorizonFootprint, padding) == 5, "Static mesh horizon footprint padding offset drifted");
    static_assert(offsetof(HorizonFootprint, footprintXY) == 8, "Static mesh horizon footprint vertices offset drifted");
    static_assert(sizeof(DistantSubset) == 128, "Distant subset ABI drifted");
    static_assert(offsetof(DistantSubset, horizonFootprint) == 64, "Distant subset horizon footprint offset drifted");
    static_assert(offsetof(DistantSubset, farFaces) == 120, "Distant subset farFaces offset drifted");
    static_assert(offsetof(DistantSubset, veryFarFaces) == 124, "Distant subset veryFarFaces offset drifted");
    static_assert(sizeof(Vec3) == 12, "Static mesh vec3 ABI drifted");
    static_assert(sizeof(BoundingSphere) == 16, "Static mesh sphere ABI drifted");
    static_assert(sizeof(Aabb) == 24, "Static mesh AABB ABI drifted");
    static_assert(sizeof(StaticMeshesFileHeader) == SerializedHeaderSize, "Static mesh header ABI drifted");
    static_assert(sizeof(StaticRecord) == SerializedStaticRecordSize, "Static mesh record ABI drifted");
    static_assert(sizeof(SubsetRecord) == SerializedSubsetRecordSize, "Static mesh subset ABI drifted");
    static_assert(sizeof(ComponentRecord) == SerializedComponentRecordSize, "Static mesh component ABI drifted");
    static_assert(sizeof(PaletteRecord) == SerializedPaletteRecordSize, "Static mesh palette ABI drifted");
    static_assert(alignof(PaletteRecord) == 4, "Static mesh palette alignment drifted");
    static_assert(std::is_standard_layout<PaletteRecord>::value, "Static mesh palette must stay POD");
    static_assert(offsetof(PaletteRecord, bound) == 0, "Static mesh palette bound offset drifted");
    static_assert(alignof(StaticMeshesFileHeader) == 8, "Static mesh header alignment drifted");
    static_assert(alignof(StaticRecord) == 4, "Static mesh record alignment drifted");
    static_assert(alignof(SubsetRecord) == 8, "Static mesh subset alignment drifted");
    static_assert(std::is_standard_layout<Vec3>::value, "Static mesh vec3 must stay POD");
    static_assert(std::is_standard_layout<BoundingSphere>::value, "Static mesh sphere must stay POD");
    static_assert(std::is_standard_layout<Aabb>::value, "Static mesh AABB must stay POD");
    static_assert(std::is_standard_layout<StaticMeshesFileHeader>::value, "Static mesh header must stay POD");
    static_assert(std::is_standard_layout<StaticRecord>::value, "Static mesh record must stay POD");
    static_assert(std::is_standard_layout<SubsetRecord>::value, "Static mesh subset must stay POD");
    static_assert(std::is_standard_layout<ComponentRecord>::value, "Static mesh component must stay POD");
    static_assert(offsetof(Vec3, x) == 0, "Static mesh vec3 x offset drifted");
    static_assert(offsetof(Vec3, y) == 4, "Static mesh vec3 y offset drifted");
    static_assert(offsetof(Vec3, z) == 8, "Static mesh vec3 z offset drifted");
    static_assert(offsetof(BoundingSphere, radius) == 0, "Static mesh sphere radius offset drifted");
    static_assert(offsetof(BoundingSphere, center) == 4, "Static mesh sphere center offset drifted");
    static_assert(offsetof(Aabb, min) == 0, "Static mesh AABB min offset drifted");
    static_assert(offsetof(Aabb, max) == 12, "Static mesh AABB max offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, version) == 8, "Static mesh header version offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, header_size) == 12, "Static mesh header size offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, static_record_size) == 16, "Static mesh static_record_size offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, subset_record_size) == 20, "Static mesh subset_record_size offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, vertex_stride) == 24, "Static mesh vertex_stride offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, index_element_size) == 28, "Static mesh index_element_size offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, static_count) == 32, "Static mesh static_count offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, subset_count) == 36, "Static mesh subset_count offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, static_table_offset) == 40, "Static mesh static_table_offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, static_table_size) == 48, "Static mesh static_table_size drifted");
    static_assert(offsetof(StaticMeshesFileHeader, subset_table_offset) == 56, "Static mesh subset_table_offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, subset_table_size) == 64, "Static mesh subset_table_size drifted");
    static_assert(offsetof(StaticMeshesFileHeader, texture_blob_offset) == 72, "Static mesh texture_blob_offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, texture_blob_size) == 80, "Static mesh texture_blob_size drifted");
    static_assert(offsetof(StaticMeshesFileHeader, geometry_blob_offset) == 88, "Static mesh geometry_blob_offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, geometry_blob_size) == 96, "Static mesh geometry_blob_size drifted");
    static_assert(offsetof(StaticMeshesFileHeader, grass_vertex_stride) == 104, "Static mesh grass_vertex_stride offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, reserved) == 108, "Static mesh reserved offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, component_table_offset) == 112, "Static mesh component_table_offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, component_table_size) == 120, "Static mesh component_table_size drifted");
    static_assert(offsetof(StaticMeshesFileHeader, component_record_size) == 128, "Static mesh component_record_size drifted");
    static_assert(offsetof(StaticMeshesFileHeader, component_count) == 132, "Static mesh component_count drifted");
    static_assert(offsetof(StaticMeshesFileHeader, palette_table_offset) == 136, "Static mesh palette_table_offset drifted");
    static_assert(offsetof(StaticMeshesFileHeader, palette_table_size) == 144, "Static mesh palette_table_size drifted");
    static_assert(offsetof(StaticMeshesFileHeader, palette_record_size) == 152, "Static mesh palette_record_size drifted");
    static_assert(offsetof(StaticMeshesFileHeader, palette_count) == 156, "Static mesh palette_count drifted");
    static_assert(offsetof(StaticRecord, static_type) == 0, "Static mesh static_type offset drifted");
    static_assert(offsetof(StaticRecord, sphere) == 4, "Static mesh sphere offset drifted");
    static_assert(offsetof(StaticRecord, aabb) == 20, "Static mesh AABB offset drifted");
    static_assert(offsetof(StaticRecord, first_subset_index) == 44, "Static mesh first_subset_index offset drifted");
    static_assert(offsetof(StaticRecord, subset_count) == 48, "Static mesh subset_count offset drifted");
    static_assert(offsetof(SubsetRecord, sphere) == 0, "Static mesh subset sphere offset drifted");
    static_assert(offsetof(SubsetRecord, aabb) == 16, "Static mesh subset AABB offset drifted");
    static_assert(offsetof(SubsetRecord, texture_path_offset) == 40, "Static mesh subset texture_path_offset drifted");
    static_assert(offsetof(SubsetRecord, vertex_offset) == 48, "Static mesh subset vertex_offset drifted");
    static_assert(offsetof(SubsetRecord, index_offset) == 56, "Static mesh subset index_offset drifted");
    static_assert(offsetof(SubsetRecord, vertex_count) == 64, "Static mesh subset vertex_count drifted");
    static_assert(offsetof(SubsetRecord, triangle_count) == 68, "Static mesh subset triangle_count drifted");
    static_assert(offsetof(SubsetRecord, flags) == 72, "Static mesh subset flags drifted");
    static_assert(offsetof(SubsetRecord, texture_path_length) == 76, "Static mesh subset texture_path_length drifted");
    static_assert(offsetof(SubsetRecord, horizonFootprint) == 80, "Static mesh subset horizonFootprint drifted");
    static_assert(offsetof(SubsetRecord, first_component_index) == 136, "Static mesh subset first_component_index drifted");
    static_assert(offsetof(SubsetRecord, component_count) == 140, "Static mesh subset component_count drifted");
    static_assert(offsetof(SubsetRecord, first_palette_index) == 144, "Static mesh subset first_palette_index drifted");
    static_assert(offsetof(SubsetRecord, palette_count) == 148, "Static mesh subset palette_count drifted");
    static_assert(offsetof(ComponentRecord, first_triangle) == 0, "Static mesh component first_triangle drifted");
    static_assert(offsetof(ComponentRecord, triangle_count) == 4, "Static mesh component triangle_count drifted");
    static_assert(offsetof(ComponentRecord, radius) == 8, "Static mesh component radius drifted");
    static_assert(offsetof(ComponentRecord, classification) == 12, "Static mesh component classification drifted");
    static_assert(offsetof(ComponentRecord, reserved) == 13, "Static mesh component reserved drifted");
    static_assert(VertexStride == SIZEOFSTATICVERT, "Static mesh vertex stride must match SIZEOFSTATICVERT");
    static_assert(GrassVertexStride == SIZEOFGRASSVERT, "Static mesh grass vertex stride must match SIZEOFGRASSVERT");

    enum class HeaderValidation {
        Ok,
        InvalidMagic,
        UnsupportedVersion,
        InvalidHeaderSize,
        InvalidStaticRecordSize,
        InvalidSubsetRecordSize,
        InvalidComponentRecordSize,
        InvalidPaletteRecordSize,
        UnsupportedVertexStride,
        UnsupportedGrassVertexStride,
        InvalidReservedField,
        UnsupportedIndexElementSize,
        InvalidStaticTable,
        InvalidSubsetTable,
        InvalidComponentTable,
        InvalidPaletteTable,
        InvalidSectionLayout,
        InvalidGeometryBlob
    };

    inline bool HasExpectedMagic(const StaticMeshesFileHeader& header) {
        return std::memcmp(header.magic, FileMagic, sizeof(FileMagic)) == 0;
    }

    inline bool TryAdd(std::uint64_t a, std::uint64_t b, std::uint64_t& result) {
        if (a > std::numeric_limits<std::uint64_t>::max() - b) {
            return false;
        }

        result = a + b;
        return true;
    }

    inline bool TryMultiply(std::uint64_t a, std::uint64_t b, std::uint64_t& result) {
        if (a != 0 && b > std::numeric_limits<std::uint64_t>::max() / a) {
            return false;
        }

        result = a * b;
        return true;
    }

    inline bool RangeWithinFile(std::uint64_t offset, std::uint64_t size, std::uint64_t fileSize) {
        if (offset > fileSize) {
            return false;
        }
        if (size > fileSize - offset) {
            return false;
        }
        return true;
    }

    inline bool TryGetVertexDataBytes(std::uint32_t vertexCount, std::uint32_t stride, std::uint64_t& result) {
        return TryMultiply(static_cast<std::uint64_t>(vertexCount), static_cast<std::uint64_t>(stride), result);
    }

    inline bool TryGetIndexDataBytes(std::uint32_t triangleCount, std::uint64_t& result) {
        return TryMultiply(static_cast<std::uint64_t>(triangleCount), static_cast<std::uint64_t>(3u * IndexElementSize), result);
    }

    inline bool HasOnlyKnownFlags(std::uint32_t flags) {
        return (flags & ~KnownFlagsMask) == 0;
    }

    inline bool IsKnownStaticType(std::uint32_t staticType) {
        switch (staticType) {
        case STATIC_AUTO:
        case STATIC_NEAR:
        case STATIC_FAR:
        case STATIC_VERY_FAR:
        case STATIC_GRASS:
        case STATIC_TREE:
        case STATIC_BUILDING:
            return true;
        default:
            return false;
        }
    }

    inline unsigned char ToRuntimeStaticType(std::uint32_t staticType) {
        return static_cast<unsigned char>(staticType);
    }

    inline bool IsKnownComponentStaticType(std::uint8_t staticType) {
        switch (staticType) {
        case STATIC_AUTO:
        case STATIC_NEAR:
        case STATIC_FAR:
        case STATIC_VERY_FAR:
        case STATIC_TREE:
        case STATIC_BUILDING:
            return true;
        default:
            return false;
        }
    }

    inline std::uint32_t VertexStrideForStaticType(const StaticMeshesFileHeader& header, std::uint32_t staticType) {
        return staticType == STATIC_GRASS ? header.grass_vertex_stride : header.vertex_stride;
    }

    inline HeaderValidation ValidateHeader(const StaticMeshesFileHeader& header, std::uint64_t fileSize) {
        if (!HasExpectedMagic(header)) {
            return HeaderValidation::InvalidMagic;
        }
        if (header.version != FileVersion) {
            return HeaderValidation::UnsupportedVersion;
        }
        if (header.header_size != SerializedHeaderSize) {
            return HeaderValidation::InvalidHeaderSize;
        }
        if (header.static_record_size != SerializedStaticRecordSize) {
            return HeaderValidation::InvalidStaticRecordSize;
        }
        if (header.subset_record_size != SerializedSubsetRecordSize) {
            return HeaderValidation::InvalidSubsetRecordSize;
        }
        if (header.component_record_size != SerializedComponentRecordSize) {
            return HeaderValidation::InvalidComponentRecordSize;
        }
        if (header.palette_record_size != SerializedPaletteRecordSize) {
            return HeaderValidation::InvalidPaletteRecordSize;
        }
        if (header.vertex_stride != VertexStride) {
            return HeaderValidation::UnsupportedVertexStride;
        }
        if (header.grass_vertex_stride != GrassVertexStride) {
            return HeaderValidation::UnsupportedGrassVertexStride;
        }
        if (header.reserved != 0) {
            return HeaderValidation::InvalidReservedField;
        }
        if (header.index_element_size != IndexElementSize) {
            return HeaderValidation::UnsupportedIndexElementSize;
        }

        std::uint64_t expectedStaticTableSize = 0;
        if (!TryMultiply(header.static_count, header.static_record_size, expectedStaticTableSize)
            || header.static_table_size != expectedStaticTableSize) {
            return HeaderValidation::InvalidStaticTable;
        }

        std::uint64_t expectedSubsetTableSize = 0;
        if (!TryMultiply(header.subset_count, header.subset_record_size, expectedSubsetTableSize)
            || header.subset_table_size != expectedSubsetTableSize) {
            return HeaderValidation::InvalidSubsetTable;
        }

        std::uint64_t expectedComponentTableSize = 0;
        if (!TryMultiply(header.component_count, header.component_record_size, expectedComponentTableSize)
            || header.component_table_size != expectedComponentTableSize) {
            return HeaderValidation::InvalidComponentTable;
        }

        std::uint64_t expectedPaletteTableSize = 0;
        if (!TryMultiply(header.palette_count, header.palette_record_size, expectedPaletteTableSize)
            || header.palette_table_size != expectedPaletteTableSize) {
            return HeaderValidation::InvalidPaletteTable;
        }

        if (header.static_table_offset != header.header_size
            || (header.subset_table_offset % 8u) != 0
            || (header.component_table_offset % 8u) != 0
            || (header.palette_table_offset % 8u) != 0
            || (header.geometry_blob_offset % 8u) != 0) {
            return HeaderValidation::InvalidSectionLayout;
        }

        std::uint64_t staticTableEnd = 0;
        std::uint64_t subsetTableEnd = 0;
        std::uint64_t componentTableEnd = 0;
        std::uint64_t paletteTableEnd = 0;
        std::uint64_t textureBlobEnd = 0;
        std::uint64_t geometryBlobEnd = 0;
        if (!TryAdd(header.static_table_offset, header.static_table_size, staticTableEnd)
            || !TryAdd(header.subset_table_offset, header.subset_table_size, subsetTableEnd)
            || !TryAdd(header.component_table_offset, header.component_table_size, componentTableEnd)
            || !TryAdd(header.palette_table_offset, header.palette_table_size, paletteTableEnd)
            || !TryAdd(header.texture_blob_offset, header.texture_blob_size, textureBlobEnd)
            || !TryAdd(header.geometry_blob_offset, header.geometry_blob_size, geometryBlobEnd)) {
            return HeaderValidation::InvalidSectionLayout;
        }

        if (!RangeWithinFile(header.static_table_offset, header.static_table_size, fileSize)
            || !RangeWithinFile(header.subset_table_offset, header.subset_table_size, fileSize)
            || !RangeWithinFile(header.component_table_offset, header.component_table_size, fileSize)
            || !RangeWithinFile(header.palette_table_offset, header.palette_table_size, fileSize)
            || !RangeWithinFile(header.texture_blob_offset, header.texture_blob_size, fileSize)
            || !RangeWithinFile(header.geometry_blob_offset, header.geometry_blob_size, fileSize)) {
            return HeaderValidation::InvalidSectionLayout;
        }

        if (staticTableEnd > header.subset_table_offset) {
            return HeaderValidation::InvalidSectionLayout;
        }
        if (subsetTableEnd > header.component_table_offset
            || componentTableEnd > header.palette_table_offset
            || paletteTableEnd > header.texture_blob_offset
            || textureBlobEnd > header.geometry_blob_offset) {
            return HeaderValidation::InvalidSectionLayout;
        }

        if (geometryBlobEnd != fileSize) {
            return HeaderValidation::InvalidGeometryBlob;
        }

        return HeaderValidation::Ok;
    }

    inline std::string PrintableMagic(const std::uint8_t magic[8]) {
        std::string result;
        result.reserve(8);
        for (int i = 0; i < 8; ++i) {
            const auto byte = magic[i];
            result.push_back((byte >= 32 && byte <= 126) ? static_cast<char>(byte) : '.');
        }
        return result;
    }

    inline const char* ValidationMessage(HeaderValidation validation) {
        switch (validation) {
        case HeaderValidation::Ok:
            return "ok";
        case HeaderValidation::InvalidMagic:
            return "static_meshes magic must be XESTAT06";
        case HeaderValidation::UnsupportedVersion:
            return "static_meshes version must be 6";
        case HeaderValidation::InvalidHeaderSize:
            return "static_meshes header_size must be 160";
        case HeaderValidation::InvalidStaticRecordSize:
            return "static_meshes static_record_size must be 52";
        case HeaderValidation::InvalidSubsetRecordSize:
            return "static_meshes subset_record_size must be 152";
        case HeaderValidation::InvalidComponentRecordSize:
            return "static_meshes component_record_size must be 16";
        case HeaderValidation::InvalidPaletteRecordSize:
            return "static_meshes palette_record_size must be 16";
        case HeaderValidation::UnsupportedVertexStride:
            return "static_meshes vertex_stride must be 20";
        case HeaderValidation::UnsupportedGrassVertexStride:
            return "static_meshes grass_vertex_stride must be 20";
        case HeaderValidation::InvalidReservedField:
            return "static_meshes reserved field must be 0";
        case HeaderValidation::UnsupportedIndexElementSize:
            return "static_meshes index_element_size must be 2";
        case HeaderValidation::InvalidStaticTable:
            return "static_meshes static table size is inconsistent";
        case HeaderValidation::InvalidSubsetTable:
            return "static_meshes subset table size is inconsistent";
        case HeaderValidation::InvalidComponentTable:
            return "static_meshes component table size is inconsistent";
        case HeaderValidation::InvalidPaletteTable:
            return "static_meshes palette table size is inconsistent";
        case HeaderValidation::InvalidSectionLayout:
            return "static_meshes section offsets are invalid or overlapping";
        case HeaderValidation::InvalidGeometryBlob:
            return "static_meshes geometry blob must end at EOF";
        default:
            return "unknown static_meshes validation error";
        }
    }

    inline std::string DetailedValidationMessage(const StaticMeshesFileHeader& header, HeaderValidation validation) {
        switch (validation) {
        case HeaderValidation::InvalidMagic:
            return "static_meshes magic must be XESTAT06 (got '" + PrintableMagic(header.magic) + "')";
        case HeaderValidation::UnsupportedVersion:
            return "static_meshes version must be 6 (got " + std::to_string(header.version) + ")";
        case HeaderValidation::InvalidHeaderSize:
            return "static_meshes header_size must be 160 (got " + std::to_string(header.header_size) + ")";
        case HeaderValidation::InvalidStaticRecordSize:
            return "static_meshes static_record_size must be 52 (got " + std::to_string(header.static_record_size) + ")";
        case HeaderValidation::InvalidSubsetRecordSize:
            return "static_meshes subset_record_size must be 152 (got " + std::to_string(header.subset_record_size) + ")";
        case HeaderValidation::InvalidComponentRecordSize:
            return "static_meshes component_record_size must be 16 (got " + std::to_string(header.component_record_size) + ")";
        case HeaderValidation::InvalidPaletteRecordSize:
            return "static_meshes palette_record_size must be 16 (got " + std::to_string(header.palette_record_size) + ")";
        case HeaderValidation::UnsupportedVertexStride:
            return "static_meshes vertex_stride must be 20 (got " + std::to_string(header.vertex_stride) + ")";
        case HeaderValidation::UnsupportedGrassVertexStride:
            return "static_meshes grass_vertex_stride must be 20 (got " + std::to_string(header.grass_vertex_stride) + ")";
        case HeaderValidation::InvalidReservedField:
            return "static_meshes reserved field must be 0 (got " + std::to_string(header.reserved) + ")";
        case HeaderValidation::UnsupportedIndexElementSize:
            return "static_meshes index_element_size must be 2 (got " + std::to_string(header.index_element_size) + ")";
        default:
            return ValidationMessage(validation);
        }
    }
}

namespace TerrainBin {
    static constexpr std::uint8_t FileMagic[8] = { 'X', 'E', 'L', 'A', 'N', 'D', '0', '2' };
    static constexpr std::uint32_t FileVersion = 2;
    static constexpr std::uint32_t SerializedHeaderSize = 116;
    static constexpr std::uint32_t VertexStride = 20;
    static constexpr std::uint32_t FileIndexFormatU32 = 1;
    static constexpr std::uint32_t FileIndexFormatAuto = 2;
    static constexpr std::uint32_t TerrainSortToken = 0;

    // Version 15 uses fixed paths for terrain.bin and every companion DDS.
    static constexpr char TerrainAtlasFilePath[] = "Data Files\\distantland\\terrain_atlas.dds";
    static constexpr char TerrainMaterialFilePath[] = "Data Files\\distantland\\terrain_material.dds";
    static constexpr char TerrainMaterialFlagsFilePath[] = "Data Files\\distantland\\terrain_material_flags.dds";
    static constexpr char TerrainPatchAlbedoFilePath[] = "Data Files\\distantland\\terrain_patch_albedo.dds";
    static constexpr char TerrainBlendPatternsFilePath[] = "Data Files\\distantland\\terrain_blend_patterns.dds";

    struct TerrainVertex {
        D3DXVECTOR3 position;
        std::uint8_t normal[4];
        DWORD color;
    };

    struct TerrainFileHeader {
        std::uint8_t magic[8];
        std::uint32_t version;
        float cellSize;
        float patchSize;
        std::int32_t originCell[2];
        std::uint32_t cellSizeXY[2];
        float worldOrigin[2];
        float worldSize[2];
        std::uint32_t atlasSize;
        std::uint32_t logicalTileSize;
        std::uint32_t gutterSize;
        std::uint32_t physicalTileSize;
        std::uint32_t tilesPerRow;
        std::uint32_t atlasMaxLod;
        std::uint32_t materialSizeXY[2];
        std::uint32_t patternCount;
        std::uint32_t patternTileSize;
        std::uint32_t patternGutterSize;
        std::uint32_t patternPhysicalSize;
        std::uint32_t patternsPerRow;
        std::uint32_t vertexStride;
        std::uint32_t fileIndexFormat;
        std::uint32_t meshCount;
    };

    struct TerrainMeshHeader {
        float sphereRadius;
        D3DXVECTOR3 sphereCenter;
        D3DXVECTOR3 boxMin;
        D3DXVECTOR3 boxMax;
        std::uint32_t vertexCount;
        std::uint32_t triangleCount;
    };

    struct TerrainMeshLayout {
        TerrainMeshHeader header;
        std::uint64_t vertexDataOffset;
        std::uint64_t vertexDataBytes;
        std::uint64_t indexDataOffset;
        std::uint64_t indexDataBytes;
    };

    static_assert(sizeof(TerrainVertex) == VertexStride, "Terrain vertex ABI drifted");
    static_assert(sizeof(TerrainFileHeader) == SerializedHeaderSize, "Terrain header ABI drifted");
    static_assert(sizeof(TerrainMeshHeader) == 48, "Terrain mesh header ABI drifted");

    class Reader {
        const std::uint8_t* begin;
        const std::uint8_t* cursor;
        const std::uint8_t* end;

    public:
        Reader(const void* data, std::size_t size)
            : begin(static_cast<const std::uint8_t*>(data)),
              cursor(begin),
              end(begin + size) {
        }

        std::size_t offset() const {
            return static_cast<std::size_t>(cursor - begin);
        }

        std::size_t remaining() const {
            return static_cast<std::size_t>(end - cursor);
        }

        bool readBytes(void* dest, std::size_t size) {
            if (remaining() < size) {
                return false;
            }

            std::memcpy(dest, cursor, size);
            cursor += size;
            return true;
        }

        bool skip(std::size_t size) {
            if (remaining() < size) {
                return false;
            }

            cursor += size;
            return true;
        }

        bool readU32(std::uint32_t& value) {
            std::uint8_t raw[4];
            if (!readBytes(raw, sizeof(raw))) {
                return false;
            }

            value = static_cast<std::uint32_t>(raw[0])
                | (static_cast<std::uint32_t>(raw[1]) << 8)
                | (static_cast<std::uint32_t>(raw[2]) << 16)
                | (static_cast<std::uint32_t>(raw[3]) << 24);
            return true;
        }

        bool readI32(std::int32_t& value) {
            std::uint32_t raw = 0;
            if (!readU32(raw)) {
                return false;
            }

            value = static_cast<std::int32_t>(raw);
            return true;
        }

        bool readF32(float& value) {
            std::uint32_t raw = 0;
            if (!readU32(raw)) {
                return false;
            }

            std::memcpy(&value, &raw, sizeof(value));
            return true;
        }

        bool readVec3(D3DXVECTOR3& value) {
            return readF32(value.x) && readF32(value.y) && readF32(value.z);
        }
    };

    enum class HeaderValidation {
        Ok,
        InvalidMagic,
        UnsupportedVersion,
        InvalidCellSize,
        InvalidPatchSize,
        InvalidRegionDimensions,
        InvalidWorldOrigin,
        InvalidWorldSize,
        InvalidAtlasLayout,
        InvalidMaterialLayout,
        InvalidPatternLayout,
        UnsupportedVertexStride,
        UnsupportedIndexFormat
    };

    inline bool HasExpectedMagic(const TerrainFileHeader& header) {
        return std::memcmp(header.magic, FileMagic, sizeof(FileMagic)) == 0;
    }

    inline bool TryMultiply(std::uint64_t a, std::uint64_t b, std::uint64_t& result) {
        if (a != 0 && b > std::numeric_limits<std::uint64_t>::max() / a) {
            return false;
        }

        result = a * b;
        return true;
    }

    inline bool TryAdd(std::uint64_t a, std::uint64_t b, std::uint64_t& result) {
        if (a > std::numeric_limits<std::uint64_t>::max() - b) {
            return false;
        }

        result = a + b;
        return true;
    }

    inline bool TryGetVertexDataBytes(std::uint32_t vertexCount, std::uint64_t& result) {
        return TryMultiply(static_cast<std::uint64_t>(vertexCount), static_cast<std::uint64_t>(VertexStride), result);
    }

    // Per-mesh index-width predicate: a mesh stores (and uploads) u16
    // triangle indices when its vertices are addressable by a 16-bit index buffer.
    // Matches the generator's mesh_uses_u16_indices and the host's uses_u16_indices.
    inline bool MeshUsesU16Indices(std::uint32_t vertexCount) {
        return vertexCount <= 0xFFFFu;
    }

    inline bool TryGetIndexDataBytes(std::uint32_t vertexCount, std::uint32_t triangleCount, std::uint64_t& result) {
        const std::uint64_t bytesPerTriangle =
            MeshUsesU16Indices(vertexCount) ? (sizeof(std::uint16_t) * 3u) : (sizeof(std::uint32_t) * 3u);
        return TryMultiply(static_cast<std::uint64_t>(triangleCount), bytesPerTriangle, result);
    }

    inline bool ReadTerrainFileHeader(Reader& reader, TerrainFileHeader& header) {
        return reader.readBytes(header.magic, sizeof(header.magic))
            && reader.readU32(header.version)
            && reader.readF32(header.cellSize)
            && reader.readF32(header.patchSize)
            && reader.readI32(header.originCell[0])
            && reader.readI32(header.originCell[1])
            && reader.readU32(header.cellSizeXY[0])
            && reader.readU32(header.cellSizeXY[1])
            && reader.readF32(header.worldOrigin[0])
            && reader.readF32(header.worldOrigin[1])
            && reader.readF32(header.worldSize[0])
            && reader.readF32(header.worldSize[1])
            && reader.readU32(header.atlasSize)
            && reader.readU32(header.logicalTileSize)
            && reader.readU32(header.gutterSize)
            && reader.readU32(header.physicalTileSize)
            && reader.readU32(header.tilesPerRow)
            && reader.readU32(header.atlasMaxLod)
            && reader.readU32(header.materialSizeXY[0])
            && reader.readU32(header.materialSizeXY[1])
            && reader.readU32(header.patternCount)
            && reader.readU32(header.patternTileSize)
            && reader.readU32(header.patternGutterSize)
            && reader.readU32(header.patternPhysicalSize)
            && reader.readU32(header.patternsPerRow)
            && reader.readU32(header.vertexStride)
            && reader.readU32(header.fileIndexFormat)
            && reader.readU32(header.meshCount);
    }

    inline bool ReadTerrainMeshHeader(Reader& reader, TerrainMeshHeader& header) {
        return reader.readF32(header.sphereRadius)
            && reader.readVec3(header.sphereCenter)
            && reader.readVec3(header.boxMin)
            && reader.readVec3(header.boxMax)
            && reader.readU32(header.vertexCount)
            && reader.readU32(header.triangleCount);
    }

    inline HeaderValidation ValidateHeader(const TerrainFileHeader& header) {
        if (!HasExpectedMagic(header)) {
            return HeaderValidation::InvalidMagic;
        }
        if (header.version != FileVersion) {
            return HeaderValidation::UnsupportedVersion;
        }
        if (!(header.cellSize > 0.0f) || !std::isfinite(header.cellSize)) {
            return HeaderValidation::InvalidCellSize;
        }
        if (!(header.patchSize > 0.0f) || !std::isfinite(header.patchSize)) {
            return HeaderValidation::InvalidPatchSize;
        }
        if (header.cellSizeXY[0] == 0 || header.cellSizeXY[1] == 0) {
            return HeaderValidation::InvalidRegionDimensions;
        }

        const float patchesPerCell = header.cellSize / header.patchSize;
        const float roundedPatchesPerCell = std::round(patchesPerCell);
        if (!(roundedPatchesPerCell > 0.0f) || std::fabs(patchesPerCell - roundedPatchesPerCell) > 0.001f) {
            return HeaderValidation::InvalidRegionDimensions;
        }

        const float expectedWorldOriginX = static_cast<float>(header.originCell[0]) * header.cellSize;
        const float expectedWorldOriginY = static_cast<float>(header.originCell[1]) * header.cellSize;
        if (std::fabs(header.worldOrigin[0] - expectedWorldOriginX) > 0.25f
            || std::fabs(header.worldOrigin[1] - expectedWorldOriginY) > 0.25f) {
            return HeaderValidation::InvalidWorldOrigin;
        }

        const float expectedWorldWidth = static_cast<float>(header.cellSizeXY[0]) * header.cellSize;
        const float expectedWorldHeight = static_cast<float>(header.cellSizeXY[1]) * header.cellSize;
        if (std::fabs(header.worldSize[0] - expectedWorldWidth) > 0.25f
            || std::fabs(header.worldSize[1] - expectedWorldHeight) > 0.25f) {
            return HeaderValidation::InvalidWorldSize;
        }

        const std::uint32_t patchGrid = static_cast<std::uint32_t>(roundedPatchesPerCell);
        const std::uint64_t expectedMaterialWidth = static_cast<std::uint64_t>(header.cellSizeXY[0]) * patchGrid;
        const std::uint64_t expectedMaterialHeight = static_cast<std::uint64_t>(header.cellSizeXY[1]) * patchGrid;
        if (header.materialSizeXY[0] == 0 || header.materialSizeXY[1] == 0
            || header.materialSizeXY[0] != expectedMaterialWidth
            || header.materialSizeXY[1] != expectedMaterialHeight) {
            return HeaderValidation::InvalidMaterialLayout;
        }

        const std::uint64_t atlasRowWidth = static_cast<std::uint64_t>(header.tilesPerRow) * header.physicalTileSize;
        if (header.atlasSize == 0
            || header.logicalTileSize == 0
            || header.physicalTileSize != header.logicalTileSize + 2u * header.gutterSize
            || header.tilesPerRow == 0
            || atlasRowWidth > header.atlasSize) {
            return HeaderValidation::InvalidAtlasLayout;
        }

        std::uint32_t expectedAtlasMaxLod = 0;
        switch (header.logicalTileSize) {
        case 64:
            expectedAtlasMaxLod = 0;
            break;
        case 128:
            expectedAtlasMaxLod = 1;
            break;
        case 256:
            expectedAtlasMaxLod = 2;
            break;
        case 512:
            expectedAtlasMaxLod = 3;
            break;
        default:
            return HeaderValidation::InvalidAtlasLayout;
        }
        if (header.atlasMaxLod != expectedAtlasMaxLod) {
            return HeaderValidation::InvalidAtlasLayout;
        }

        if (header.patternCount == 0
            || header.patternCount > 256
            || header.patternTileSize == 0
            || header.patternPhysicalSize != header.patternTileSize + 2u * header.patternGutterSize
            || header.patternsPerRow == 0) {
            return HeaderValidation::InvalidPatternLayout;
        }

        if (header.vertexStride != VertexStride) {
            return HeaderValidation::UnsupportedVertexStride;
        }
        if (header.fileIndexFormat != FileIndexFormatAuto) {
            return HeaderValidation::UnsupportedIndexFormat;
        }

        return HeaderValidation::Ok;
    }

    inline const char* ValidationMessage(HeaderValidation validation) {
        switch (validation) {
        case HeaderValidation::Ok:
            return "ok";
        case HeaderValidation::InvalidMagic:
            return "terrain.bin magic must be XELAND02";
        case HeaderValidation::UnsupportedVersion:
            return "terrain.bin version must be 2";
        case HeaderValidation::InvalidCellSize:
            return "terrain.bin cell_size must be finite and positive";
        case HeaderValidation::InvalidPatchSize:
            return "terrain.bin patch_size must be finite and positive";
        case HeaderValidation::InvalidRegionDimensions:
            return "terrain.bin region dimensions are inconsistent";
        case HeaderValidation::InvalidWorldOrigin:
            return "terrain.bin world_origin must match origin_cell * cell_size";
        case HeaderValidation::InvalidWorldSize:
            return "terrain.bin world_size must match cell_size_xy * cell_size";
        case HeaderValidation::InvalidAtlasLayout:
            return "terrain.bin source atlas layout fields are inconsistent";
        case HeaderValidation::InvalidMaterialLayout:
            return "terrain.bin material_size_xy must match the patch grid";
        case HeaderValidation::InvalidPatternLayout:
            return "terrain.bin blend-pattern layout fields are inconsistent";
        case HeaderValidation::UnsupportedVertexStride:
            return "terrain.bin vertex_stride must be 20";
        case HeaderValidation::UnsupportedIndexFormat:
            return "terrain.bin file_index_format must be 2 (per-mesh u16/u32 inferred from vertex_count)";
        default:
            return "unknown terrain.bin validation error";
        }
    }

    inline std::string PrintableMagic(const std::uint8_t magic[8]) {
        std::string result;
        result.reserve(8);
        for (int i = 0; i < 8; ++i) {
            const auto byte = magic[i];
            result.push_back((byte >= 32 && byte <= 126) ? static_cast<char>(byte) : '.');
        }
        return result;
    }

    inline std::string DetailedValidationMessage(const TerrainFileHeader& header, HeaderValidation validation) {
        switch (validation) {
        case HeaderValidation::InvalidMagic:
            return "terrain.bin magic must be XELAND02 (got '" + PrintableMagic(header.magic) + "')";
        case HeaderValidation::UnsupportedVersion:
            return "terrain.bin version must be 2 (got " + std::to_string(header.version) + ")";
        case HeaderValidation::UnsupportedVertexStride:
            return "terrain.bin vertex_stride must be 20 (got " + std::to_string(header.vertexStride) + ")";
        case HeaderValidation::UnsupportedIndexFormat:
            return "terrain.bin file_index_format must be 2 (per-mesh u16/u32 inferred from vertex_count, got " + std::to_string(header.fileIndexFormat) + ")";
        case HeaderValidation::InvalidAtlasLayout:
            return std::string("terrain.bin source atlas layout fields are inconsistent (atlas_size=")
                + std::to_string(header.atlasSize)
                + ", logical_tile_size=" + std::to_string(header.logicalTileSize)
                + ", gutter_size=" + std::to_string(header.gutterSize)
                + ", physical_tile_size=" + std::to_string(header.physicalTileSize)
                + ", tiles_per_row=" + std::to_string(header.tilesPerRow)
                + ", atlas_max_lod=" + std::to_string(header.atlasMaxLod)
                + "); logical_tile_size must be 64, 128, 256, or 512, physical_tile_size must equal"
                + " logical_tile_size + 2*gutter_size, and tiles_per_row*physical_tile_size must not exceed atlas_size";
        default:
            return ValidationMessage(validation);
        }
    }

    inline bool ReadTerrainMeshLayout(Reader& reader, TerrainMeshLayout& layout) {
        if (!ReadTerrainMeshHeader(reader, layout.header)) {
            return false;
        }
        if (!TryGetVertexDataBytes(layout.header.vertexCount, layout.vertexDataBytes)
            || !TryGetIndexDataBytes(layout.header.vertexCount, layout.header.triangleCount, layout.indexDataBytes)) {
            return false;
        }

        if (layout.vertexDataBytes > static_cast<std::uint64_t>(std::numeric_limits<std::size_t>::max())
            || layout.indexDataBytes > static_cast<std::uint64_t>(std::numeric_limits<std::size_t>::max())) {
            return false;
        }

        layout.vertexDataOffset = static_cast<std::uint64_t>(reader.offset());
        if (!reader.skip(static_cast<std::size_t>(layout.vertexDataBytes))) {
            return false;
        }

        layout.indexDataOffset = static_cast<std::uint64_t>(reader.offset());
        return reader.skip(static_cast<std::size_t>(layout.indexDataBytes));
    }

    inline bool ReadTerrainFileLayouts(Reader& reader, const TerrainFileHeader& header, std::vector<TerrainMeshLayout>& layouts) {
        layouts.clear();
        if (header.meshCount > reader.remaining() / sizeof(TerrainMeshHeader)) {
            return false;
        }
        layouts.reserve(header.meshCount);
        for (std::uint32_t i = 0; i < header.meshCount; ++i) {
            TerrainMeshLayout layout = {};
            if (!ReadTerrainMeshLayout(reader, layout)) {
                return false;
            }
            layouts.push_back(layout);
        }
        return true;
    }

    template <class ReadAt>
    inline bool ReadTerrainFileLayoutsMapped(
        const TerrainFileHeader& header,
        std::uint64_t fileSize,
        ReadAt&& readAt,
        std::vector<TerrainMeshLayout>& layouts,
        std::uint64_t& finalCursor
    ) {
        layouts.clear();
        if (fileSize < SerializedHeaderSize) {
            return false;
        }
        if (header.meshCount > (fileSize - SerializedHeaderSize) / sizeof(TerrainMeshHeader)) {
            return false;
        }
        layouts.reserve(header.meshCount);

        std::uint64_t cursor = SerializedHeaderSize;
        for (std::uint32_t i = 0; i < header.meshCount; ++i) {
            TerrainMeshLayout layout = {};
            if (!readAt(cursor, &layout.header, sizeof(layout.header))) {
                return false;
            }
            cursor += sizeof(layout.header);

            if (!TryGetVertexDataBytes(layout.header.vertexCount, layout.vertexDataBytes)
                || !TryGetIndexDataBytes(layout.header.vertexCount, layout.header.triangleCount, layout.indexDataBytes)) {
                return false;
            }

            layout.vertexDataOffset = cursor;
            if (!TryAdd(cursor, layout.vertexDataBytes, cursor)) {
                return false;
            }

            layout.indexDataOffset = cursor;
            if (!TryAdd(cursor, layout.indexDataBytes, cursor)) {
                return false;
            }

            if (cursor > fileSize) {
                return false;
            }

            layouts.push_back(layout);
        }

        finalCursor = cursor;
        return true;
    }
}
