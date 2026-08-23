#define _CONF

#pragma setlocale("C")

#include <cstddef>
#include <cstring>

#include "support/winheader.h"

#include "configuration.h"
#include "inidata.h"
#include "../mge_config_ffi.h"
#include "support/log.h"

ConfigurationStruct Configuration;

namespace {
MgeConfigDoc* configDocument = nullptr;

void logConfigDiagnostic(const char* operation, const char* path = nullptr) {
    char diagnostic[2048] = {};
    if (configDocument &&
        mge_config_last_error(configDocument, diagnostic, sizeof(diagnostic)) == MgeConfigStatus::Ok &&
        diagnostic[0]) {
        if (path) {
            LOG::logline("Configuration %s failed at %s: %s", operation, path, diagnostic);
        } else {
            LOG::logline("Configuration %s failed: %s", operation, diagnostic);
        }
    } else if (path) {
        LOG::logline("Configuration %s failed at %s.", operation, path);
    } else {
        LOG::logline("Configuration %s failed.", operation);
    }
}

void logConfigWarnings() {
    char diagnostic[2048] = {};
    if (configDocument &&
        mge_config_last_error(configDocument, diagnostic, sizeof(diagnostic)) == MgeConfigStatus::Ok &&
        diagnostic[0]) {
        LOG::logline("Configuration validation warning: %s", diagnostic);
    }
}

bool utf8cpyToA_s(char* destination, size_t capacity, const char* source) {
    const int wideLength = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, source, -1, nullptr, 0);
    if (wideLength <= 0) {
        return false;
    }
    WCHAR* wide = new WCHAR[wideLength];
    const int converted = MultiByteToWideChar(
        CP_UTF8, MB_ERR_INVALID_CHARS, source, -1, wide, wideLength);
    const int output = converted > 0
        ? WideCharToMultiByte(
              CP_ACP, 0, wide, -1, destination, static_cast<int>(capacity), nullptr, nullptr)
        : 0;
    delete[] wide;
    return output > 0;
}

void* stagedDestination(ConfigurationStruct& staged, const iniSetting& setting) {
    const ptrdiff_t offset =
        static_cast<const char*>(setting.variable) - reinterpret_cast<const char*>(&Configuration);
    return reinterpret_cast<char*>(&staged) + offset;
}

bool applyDocument(ConfigurationStruct& target) {
    ConfigurationStruct staged = target;
    char stringBuffer[4096] = {};

    for (int i = 0; i != countof(iniSettings); ++i) {
        const iniSetting& setting = iniSettings[i];
        void* destination = stagedDestination(staged, setting);

        if (setting.type == t_string) {
            if (mge_config_get_str(
                    configDocument,
                    setting.path,
                    stringBuffer,
                    sizeof(stringBuffer)) != MgeConfigStatus::Ok ||
                !utf8cpyToA_s(
                    static_cast<char*>(destination), setting.bit_size, stringBuffer)) {
                logConfigDiagnostic("load", setting.path);
                return false;
            }
            continue;
        }
        if (setting.type == t_set) {
            if (mge_config_get_lines(
                    configDocument,
                    setting.path,
                    static_cast<char*>(destination),
                    setting.bit_size) != MgeConfigStatus::Ok) {
                logConfigDiagnostic("load", setting.path);
                return false;
            }
            continue;
        }

        double value = 0.0;
        if (mge_config_get_num(configDocument, setting.path, &value) != MgeConfigStatus::Ok) {
            logConfigDiagnostic("load", setting.path);
            return false;
        }
        switch (setting.type) {
        case t_bit:
            if (value == 1.0) {
                *static_cast<int*>(destination) |= (1 << setting.bit_size);
            } else {
                *static_cast<int*>(destination) &= ~(1 << setting.bit_size);
            }
            break;
        case t_bool:
            *static_cast<bool*>(destination) = value == 1.0;
            break;
        case t_uint8:
            *static_cast<unsigned __int8*>(destination) = static_cast<unsigned __int8>(value);
            break;
        case t_int8:
            *static_cast<signed __int8*>(destination) = static_cast<signed __int8>(value);
            break;
        case t_uint16:
            *static_cast<unsigned __int16*>(destination) = static_cast<unsigned __int16>(value);
            break;
        case t_int16:
            *static_cast<signed __int16*>(destination) = static_cast<signed __int16>(value);
            break;
        case t_uint32:
            *static_cast<unsigned __int32*>(destination) = static_cast<unsigned __int32>(value);
            break;
        case t_int32:
            *static_cast<signed __int32*>(destination) = static_cast<signed __int32>(value);
            break;
        case t_float:
            *static_cast<float*>(destination) = static_cast<float>(value);
            break;
        case t_double:
            *static_cast<double*>(destination) = value;
            break;
        case t_string:
        case t_set:
            break;
        }
    }

    target = staged;
    return true;
}

double currentNumber(const iniSetting& setting) {
    switch (setting.type) {
    case t_bit:
        return ((*static_cast<int*>(setting.variable) >> setting.bit_size) & 1) ? 1.0 : 0.0;
    case t_bool:
        return *static_cast<bool*>(setting.variable) ? 1.0 : 0.0;
    case t_uint8:
        return *static_cast<unsigned __int8*>(setting.variable);
    case t_int8:
        return *static_cast<signed __int8*>(setting.variable);
    case t_uint16:
        return *static_cast<unsigned __int16*>(setting.variable);
    case t_int16:
        return *static_cast<signed __int16*>(setting.variable);
    case t_uint32:
        return *static_cast<unsigned __int32*>(setting.variable);
    case t_int32:
        return *static_cast<signed __int32*>(setting.variable);
    case t_float:
        return *static_cast<float*>(setting.variable);
    case t_double:
        return *static_cast<double*>(setting.variable);
    case t_string:
    case t_set:
        return 0.0;
    }
    return 0.0;
}
}

bool ConfigurationStruct::LoadSettings() {
    if (configDocument) {
        return applyDocument(*this);
    }

    const MgeConfigStatus status = mge_config_open("mgeXE.toml", &configDocument);
    if (!configDocument) {
        LOG::logline("Configuration open failed with status %u.", static_cast<unsigned>(status));
        return false;
    }
    if (status == MgeConfigStatus::InvalidDefaults) {
        logConfigDiagnostic("open");
        LOG::logline("Using built-in defaults; configuration writes are disabled until a successful reload.");
    } else if (status != MgeConfigStatus::Ok && status != MgeConfigStatus::MissingDefaults) {
        logConfigDiagnostic("open");
        return false;
    }
    if (status == MgeConfigStatus::Ok) {
        logConfigWarnings();
    }
    return applyDocument(*this);
}

bool ConfigurationStruct::ReloadSettings() {
    if (!configDocument) {
        return false;
    }
    if (mge_config_reload(configDocument) != MgeConfigStatus::Ok) {
        logConfigDiagnostic("reload");
        return false;
    }
    logConfigWarnings();
    return applyDocument(*this);
}

bool ConfigurationStruct::SaveSettings() {
    if (!configDocument) {
        return false;
    }
    for (int i = 0; i != countof(iniSettings); ++i) {
        const iniSetting& setting = iniSettings[i];
        if (setting.flags & DONT_SAVE) {
            continue;
        }
        MgeConfigStatus status = MgeConfigStatus::Ok;
        if (setting.type == t_string) {
            status = mge_config_set_str(
                configDocument, setting.path, static_cast<const char*>(setting.variable));
        } else if (setting.type != t_set) {
            status = mge_config_set_num(
                configDocument, setting.path, currentNumber(setting));
        }
        if (status != MgeConfigStatus::Ok) {
            logConfigDiagnostic("stage save", setting.path);
            return false;
        }
    }
    if (mge_config_save(configDocument) != MgeConfigStatus::Ok) {
        logConfigDiagnostic("save");
        return false;
    }
    return true;
}

bool ConfigurationStruct::EnsureSettingsFile() {
    if (!configDocument || !mge_config_needs_creation(configDocument)) {
        return true;
    }
    if (mge_config_save(configDocument) != MgeConfigStatus::Ok) {
        logConfigDiagnostic("first-run creation");
        return false;
    }
    return true;
}
