#include "dlformat.h"
#include <cassert>
#include <vector>
#include <cstring>
#include <iostream>

namespace TerrainBin {
    namespace Test {
        void push_u32(std::vector<std::uint8_t>& bytes, std::uint32_t value) {
            bytes.push_back(static_cast<std::uint8_t>(value & 0xFF));
            bytes.push_back(static_cast<std::uint8_t>((value >> 8) & 0xFF));
            bytes.push_back(static_cast<std::uint8_t>((value >> 16) & 0xFF));
            bytes.push_back(static_cast<std::uint8_t>((value >> 24) & 0xFF));
        }

        void push_u16(std::vector<std::uint8_t>& bytes, std::uint16_t value) {
            bytes.push_back(static_cast<std::uint8_t>(value & 0xFF));
            bytes.push_back(static_cast<std::uint8_t>((value >> 8) & 0xFF));
        }

        void push_i32(std::vector<std::uint8_t>& bytes, std::int32_t value) {
            push_u32(bytes, static_cast<std::uint32_t>(value));
        }

        void push_f32(std::vector<std::uint8_t>& bytes, float value) {
            std::uint32_t raw;
            std::memcpy(&raw, &value, sizeof(value));
            push_u32(bytes, raw);
        }

        void push_vec3(std::vector<std::uint8_t>& bytes, float x, float y, float z) {
            push_f32(bytes, x);
            push_f32(bytes, y);
            push_f32(bytes, z);
        }

        std::vector<std::uint8_t> build_minimal_fixture() {
            std::vector<std::uint8_t> bytes;
            bytes.insert(bytes.end(), FileMagic, FileMagic + sizeof(FileMagic));
            push_u32(bytes, FileVersion);
            push_f32(bytes, 8192.0f);   // cellSize
            push_f32(bytes, 512.0f);    // patchSize
            push_i32(bytes, -40);       // originCell[0]
            push_i32(bytes, -32);       // originCell[1]
            push_u32(bytes, 1);         // cellSizeXY[0]
            push_u32(bytes, 1);         // cellSizeXY[1]
            push_f32(bytes, -327680.0f); // worldOrigin[0]
            push_f32(bytes, -262144.0f); // worldOrigin[1]
            push_f32(bytes, 8192.0f);   // worldSize[0]
            push_f32(bytes, 8192.0f);   // worldSize[1]
            push_u32(bytes, 1024);      // atlasSize
            push_u32(bytes, 64);        // logicalTileSize
            push_u32(bytes, 4);         // gutterSize
            push_u32(bytes, 72);        // physicalTileSize
            push_u32(bytes, 8);         // tilesPerRow
            push_u32(bytes, 0);         // atlasMaxLod
            push_u32(bytes, 16);        // materialSizeXY[0]
            push_u32(bytes, 16);        // materialSizeXY[1]
            push_u32(bytes, 11);        // patternCount
            push_u32(bytes, 32);        // patternTileSize
            push_u32(bytes, 2);         // patternGutterSize
            push_u32(bytes, 36);        // patternPhysicalSize
            push_u32(bytes, 4);         // patternsPerRow
            push_u32(bytes, VertexStride); // vertexStride
            push_u32(bytes, FileIndexFormatAuto); // fileIndexFormat
            push_u32(bytes, 1);         // meshCount

            // Mesh header
            push_f32(bytes, 4.0f);      // sphereRadius
            push_vec3(bytes, 1.0f, 2.0f, 3.0f); // sphereCenter
            push_vec3(bytes, 0.0f, 0.0f, 0.0f); // boxMin
            push_vec3(bytes, 2.0f, 2.0f, 4.0f); // boxMax
            push_u32(bytes, 3);         // vertexCount
            push_u32(bytes, 1);         // triangleCount

            // Vertex 0
            push_vec3(bytes, 0.0f, 0.0f, 0.0f);
            bytes.push_back(128); bytes.push_back(128); bytes.push_back(255); bytes.push_back(0); // normal
            push_u32(bytes, 0xFF3366CC); // color

            // Vertex 1
            push_vec3(bytes, 2.0f, 0.0f, 0.0f);
            bytes.push_back(255); bytes.push_back(128); bytes.push_back(128); bytes.push_back(0); // normal
            push_u32(bytes, 0xFFCC8844); // color

            // Vertex 2
            push_vec3(bytes, 0.0f, 2.0f, 4.0f);
            bytes.push_back(128); bytes.push_back(255); bytes.push_back(128); bytes.push_back(0); // normal
            push_u32(bytes, 0xFF112233); // color

            // Indices (vertexCount 3 <= 0xFFFF, so AUTO stores u16 triples)
            push_u16(bytes, 0);
            push_u16(bytes, 1);
            push_u16(bytes, 2);

            return bytes;
        }

        void run_minimal_fixture_test() {
            auto bytes = build_minimal_fixture();
            Reader reader(bytes.data(), bytes.size());
            TerrainFileHeader header;
            assert(ReadTerrainFileHeader(reader, header));
            
            HeaderValidation validation = ValidateHeader(header);
            if (validation != HeaderValidation::Ok) {
                std::cerr << "Validation failed: " << ValidationMessage(validation) << std::endl;
            }
            assert(validation == HeaderValidation::Ok);

            std::vector<TerrainMeshLayout> layouts;
            assert(ReadTerrainFileLayouts(reader, header, layouts));
            assert(layouts.size() == 1);
            assert(layouts[0].header.vertexCount == 3);
            assert(layouts[0].header.triangleCount == 1);
            
            std::cout << "terrain.bin minimal fixture test passed" << std::endl;
        }
    }
}

namespace StaticMeshesBin {
    namespace Test {
        // A v3 header describing a valid two-static (one regular, one grass) file.
        // ValidateHeader only inspects header fields against fileSize, so the section
        // bytes themselves are not materialized here.
        StaticMeshesFileHeader build_valid_header() {
            StaticMeshesFileHeader header = {};
            std::memcpy(header.magic, FileMagic, sizeof(FileMagic));
            header.version = FileVersion;
            header.header_size = SerializedHeaderSize;
            header.static_record_size = SerializedStaticRecordSize;
            header.subset_record_size = SerializedSubsetRecordSize;
            header.vertex_stride = VertexStride;
            header.index_element_size = IndexElementSize;
            header.static_count = 2;
            header.subset_count = 2;
            header.static_table_offset = SerializedHeaderSize;                 // 112
            header.static_table_size = 2ull * SerializedStaticRecordSize;      // 104
            header.subset_table_offset = 216;                                  // 112 + 104
            header.subset_table_size = 2ull * SerializedSubsetRecordSize;      // 160
            header.texture_blob_offset = 376;                                  // 216 + 160
            header.texture_blob_size = 4;                                      // "a\0b\0"
            header.geometry_blob_offset = 384;                                 // 8-aligned, after texture blob
            header.geometry_blob_size = 156;                                   // (3*28 + 6) + (3*20 + 6)
            header.grass_vertex_stride = GrassVertexStride;
            header.reserved = 0;
            return header;
        }

        const std::uint64_t kFileSize = 540;   // geometry_blob_offset + geometry_blob_size

        void run_header_validation_test() {
            auto header = build_valid_header();
            HeaderValidation validation = ValidateHeader(header, kFileSize);
            if (validation != HeaderValidation::Ok) {
                std::cerr << "Validation failed: " << DetailedValidationMessage(header, validation) << std::endl;
            }
            assert(validation == HeaderValidation::Ok);

            { auto h = header; h.magic[7] = '2'; assert(ValidateHeader(h, kFileSize) == HeaderValidation::InvalidMagic); }
            { auto h = header; h.version = 2; assert(ValidateHeader(h, kFileSize) == HeaderValidation::UnsupportedVersion); }
            { auto h = header; h.header_size = 104; assert(ValidateHeader(h, kFileSize) == HeaderValidation::InvalidHeaderSize); }
            { auto h = header; h.vertex_stride = 20; assert(ValidateHeader(h, kFileSize) == HeaderValidation::UnsupportedVertexStride); }
            { auto h = header; h.grass_vertex_stride = 28; assert(ValidateHeader(h, kFileSize) == HeaderValidation::UnsupportedGrassVertexStride); }
            { auto h = header; h.reserved = 1; assert(ValidateHeader(h, kFileSize) == HeaderValidation::InvalidReservedField); }

            std::cout << "static_meshes header validation test passed" << std::endl;
        }

        void run_stride_selection_test() {
            auto header = build_valid_header();

            assert(VertexStrideForStaticType(header, STATIC_AUTO) == VertexStride);
            assert(VertexStrideForStaticType(header, STATIC_NEAR) == VertexStride);
            assert(VertexStrideForStaticType(header, STATIC_FAR) == VertexStride);
            assert(VertexStrideForStaticType(header, STATIC_VERY_FAR) == VertexStride);
            assert(VertexStrideForStaticType(header, STATIC_TREE) == VertexStride);
            assert(VertexStrideForStaticType(header, STATIC_BUILDING) == VertexStride);
            assert(VertexStrideForStaticType(header, STATIC_GRASS) == GrassVertexStride);

            std::uint64_t bytes = 0;
            assert(TryGetVertexDataBytes(3, VertexStride, bytes) && bytes == 3ull * 28);
            assert(TryGetVertexDataBytes(3, GrassVertexStride, bytes) && bytes == 3ull * 20);

            assert(IsKnownStaticType(STATIC_GRASS));
            assert(!IsKnownStaticType(7));

            std::cout << "static_meshes stride selection test passed" << std::endl;
        }
    }
}

// Optional: a simple entry point if we want to run this as a standalone tool
// int main() {
//     TerrainBin::Test::run_minimal_fixture_test();
//     StaticMeshesBin::Test::run_header_validation_test();
//     StaticMeshesBin::Test::run_stride_selection_test();
//     return 0;
// }
