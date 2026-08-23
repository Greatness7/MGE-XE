// Focused version-16 output contract harness.
#include "mge/mgeversion.h"

#include <cstdint>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <limits>
#include <vector>

namespace fs = std::filesystem;

namespace {

bool readExact(const fs::path& path, std::vector<std::uint8_t>& out) {
    std::ifstream input(path, std::ios::binary);
    if (!input) {
        return false;
    }
    input.seekg(0, std::ios::end);
    const auto size = static_cast<std::size_t>(input.tellg());
    input.seekg(0, std::ios::beg);
    out.resize(size);
    return static_cast<bool>(input.read(reinterpret_cast<char*>(out.data()), static_cast<std::streamsize>(size)));
}

bool startsWith(const std::vector<std::uint8_t>& bytes, const char* magic, std::size_t length) {
    if (bytes.size() < length) {
        return false;
    }
    for (std::size_t i = 0; i < length; ++i) {
        if (bytes[i] != static_cast<std::uint8_t>(magic[i])) {
            return false;
        }
    }
    return true;
}

std::uint32_t readU32Le(const std::vector<std::uint8_t>& bytes, std::size_t offset) {
    return static_cast<std::uint32_t>(bytes[offset])
        | (static_cast<std::uint32_t>(bytes[offset + 1]) << 8)
        | (static_cast<std::uint32_t>(bytes[offset + 2]) << 16)
        | (static_cast<std::uint32_t>(bytes[offset + 3]) << 24);
}

int fail(const char* message) {
    std::fprintf(stderr, "output_contract_test: %s\n", message);
    return 1;
}

} // namespace

extern "C" int mge_dl_contract_main(int argc, char** argv) {
    static_assert(MGE_DL_VERSION == 16, "C++ client MGE_DL_VERSION must be 16");
    if (MGE_DL_VERSION != 16) {
        return fail("MGE_DL_VERSION is not 16");
    }

    if (argc < 2) {
        std::printf("output_contract_test: version constant OK (%u)\n", MGE_DL_VERSION);
        return 0;
    }

    const fs::path root = argv[1];
    const fs::path versionPath = root / "distantland" / "version";
    const fs::path usagePath = root / "distantland" / "statics" / "usage.data";
    const fs::path terrainPath = root / "distantland" / "terrain.bin";

    std::vector<std::uint8_t> versionBytes;
    if (!readExact(versionPath, versionBytes) || versionBytes.size() != 1) {
        return fail("version file missing or malformed");
    }
    if (versionBytes[0] != MGE_DL_VERSION) {
        return fail("version byte does not match MGE_DL_VERSION");
    }
    std::uint64_t staticCountSum = 0;
    for (unsigned shardId = 0; shardId < MGE_STATIC_MESH_SHARD_COUNT; ++shardId) {
        char shardName[32] = {};
        std::snprintf(
            shardName,
            sizeof(shardName),
            "static_meshes_%0*u",
            MGE_STATIC_MESH_SHARD_ID_WIDTH,
            shardId
        );
        const fs::path staticPath = root / "distantland" / "statics" / shardName;
        std::vector<std::uint8_t> staticBytes;
        if (!readExact(staticPath, staticBytes) || staticBytes.size() < 36) {
            return fail("static shard missing or truncated");
        }
        if (!startsWith(staticBytes, "XESTAT05", 8) || readU32Le(staticBytes, 8) != 5) {
            return fail("static shard header mismatch");
        }
        const std::uint32_t shardStaticCount = readU32Le(staticBytes, 32);
        if (staticCountSum > std::numeric_limits<std::uint32_t>::max() - shardStaticCount) {
            return fail("static shard header-count sum overflows u32");
        }
        staticCountSum += shardStaticCount;
    }

    std::vector<std::uint8_t> usageBytes;
    if (!readExact(usagePath, usageBytes) || usageBytes.size() < 4) {
        return fail("usage.data missing or truncated");
    }
    if (readU32Le(usageBytes, 0) != staticCountSum) {
        return fail("usage.data count does not match static shard header sum");
    }

    std::vector<std::uint8_t> terrainBytes;
    if (!readExact(terrainPath, terrainBytes) || terrainBytes.size() < 12) {
        return fail("terrain.bin missing or truncated");
    }
    if (!startsWith(terrainBytes, "XELAND02", 8) || readU32Le(terrainBytes, 8) != 2) {
        return fail("terrain.bin header mismatch");
    }

    std::printf("output_contract_test: OK for %s\n", root.string().c_str());
    return 0;
}
