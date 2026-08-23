#pragma once
#ifdef _CONF
#ifndef _CONFIGINTERNAL_H_
#define _CONFIGINTERNAL_H_

#ifndef countof
template <typename T, size_t N>
char (&_ArraySizeHelper(T (&array)[N]))[N];
#define countof(array) (sizeof(_ArraySizeHelper(array)))
#endif

#define DONT_SAVE_BIT 7
#define DONT_SAVE MASK(DONT_SAVE_BIT)

enum vtype {
    t_bit,
    t_bool,
    t_uint8,
    t_int8,
    t_uint16,
    t_int16,
    t_uint32,
    t_int32,
    t_float,
    t_double,
    t_string,
    t_set
};

struct iniSetting {
    void* variable;
    vtype type;
    size_t bit_size;
    const char* path;
    DWORD flags;
};

#endif /* _CONFIGINTERNAL_H_ */
#endif /* _CONF */
