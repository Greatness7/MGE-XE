#include <cstdarg>
#include <cstdio>

#include "mge/configuration.cpp"

namespace LOG {
bool open(const char*) {
    return true;
}

std::size_t write(const char*) {
    return 0;
}

std::size_t log(const char*, ...) {
    return 0;
}

std::size_t logline(const char* format, ...) {
    va_list args;
    va_start(args, format);
    const int written = std::vfprintf(stderr, format, args);
    va_end(args);
    std::fputc('\n', stderr);
    return written > 0 ? static_cast<std::size_t>(written) : 0;
}

std::size_t logbinary(void*, std::size_t) {
    return 0;
}

std::size_t winerror(const char*, ...) {
    return 0;
}

void flush() {}
void close() {}
} // namespace LOG

extern "C" void __cdecl mge_log_panic(const char* message) noexcept {
    LOG::logline("%s", message);
}

extern "C" std::size_t mge_config_contract_binding_count() {
    return countof(iniSettings);
}

extern "C" const char* mge_config_contract_binding_path(std::size_t index) {
    return index < countof(iniSettings) ? iniSettings[index].path : nullptr;
}

extern "C" unsigned mge_config_contract_binding_type(std::size_t index) {
    return index < countof(iniSettings) ? static_cast<unsigned>(iniSettings[index].type) : ~0u;
}

extern "C" std::size_t mge_config_contract_binding_size(std::size_t index) {
    return index < countof(iniSettings) ? iniSettings[index].bit_size : 0;
}

extern "C" unsigned long mge_config_contract_binding_flags(std::size_t index) {
    return index < countof(iniSettings) ? iniSettings[index].flags : 0;
}

extern "C" double mge_config_contract_binding_number(std::size_t index) {
    return index < countof(iniSettings) ? currentNumber(iniSettings[index]) : 0.0;
}

extern "C" const char* mge_config_contract_binding_buffer(std::size_t index) {
    if (index >= countof(iniSettings)) {
        return nullptr;
    }
    const iniSetting& setting = iniSettings[index];
    return setting.type == t_string || setting.type == t_set
        ? static_cast<const char*>(setting.variable)
        : nullptr;
}

extern "C" int mge_config_contract_load_defaults() {
    return Configuration.LoadSettings() ? 1 : 0;
}

extern "C" const void* mge_config_contract_configuration_address() {
    return &Configuration;
}

extern "C" const void* mge_config_contract_dl_address() {
    return &Configuration.DL;
}

extern "C" void mge_config_contract_set_camera_zoom(float value) {
    Configuration.CameraEffects.zoom = value;
}

extern "C" float mge_config_contract_camera_zoom() {
    return Configuration.CameraEffects.zoom;
}
