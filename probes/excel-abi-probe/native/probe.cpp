#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif

#ifndef NOMINMAX
#define NOMINMAX
#endif

#include <Windows.h>
#include <XLCALL.H>

#include <cstddef>
#include <limits>

// xlfRegister's B, A, J and D% prototypes. On Windows `long` is 32 bits
// on both x86 and x64; bool must never replace the two-byte `short`.
static_assert(sizeof(double) == 8);
static_assert(sizeof(short) == 2);
static_assert(sizeof(long) == 4);
static_assert(sizeof(unsigned short) == 2);
using NativeArgumentReceiver = LPXLOPER12(__stdcall*)(
    double, short, long, const unsigned short*, LPXLOPER12);

extern "C" int xlfn_probe_native_arguments(NativeArgumentReceiver receive) {
    const unsigned short text[] = {5, L'\u65e5', 0, 0xd83d, 0xde00, L'x'};
    for (short boolean = 0; boolean <= 1; ++boolean) {
        XLOPER12 result{};
        const long integer = boolean ? (std::numeric_limits<long>::max)()
                                     : (std::numeric_limits<long>::min)();
        const auto returned = receive(123.5, boolean, integer, text, &result);
        if (returned != &result || result.xltype != xltypeInt || result.val.w != 31) {
            return 0;
        }
    }
    return 1;
}

extern "C" std::size_t xlfn_probe_fp12_size() {
    return sizeof(FP12);
}

extern "C" std::size_t xlfn_probe_fp12_align() {
    return alignof(FP12);
}

extern "C" std::size_t xlfn_probe_fp12_array_offset() {
    return offsetof(FP12, array);
}

extern "C" std::size_t xlfn_probe_xloper12_size() {
    return sizeof(XLOPER12);
}

extern "C" std::size_t xlfn_probe_xloper12_align() {
    return alignof(XLOPER12);
}

extern "C" std::size_t xlfn_probe_xloper12_xltype_offset() {
    return offsetof(XLOPER12, xltype);
}

extern "C" std::size_t xlfn_probe_xloper12_array_size() {
    return sizeof(((XLOPER12*)nullptr)->val.array);
}

extern "C" std::size_t xlfn_probe_xloper12_sref_size() {
    return sizeof(((XLOPER12*)nullptr)->val.sref);
}

extern "C" std::size_t xlfn_probe_idsheet_size() {
    return sizeof(IDSHEET);
}

extern "C" std::size_t xlfn_probe_xloper12_mref_size() {
    return sizeof(((XLOPER12*)nullptr)->val.mref);
}

extern "C" std::size_t xlfn_probe_xloper12_mref_idsheet_offset() {
    return offsetof(decltype(((XLOPER12*)nullptr)->val.mref), idSheet);
}

extern "C" std::size_t xlfn_probe_xloper12_flow_size() {
    return sizeof(((XLOPER12*)nullptr)->val.flow);
}

extern "C" std::size_t xlfn_probe_xloper12_flow_value_size() {
    return sizeof(((XLOPER12*)nullptr)->val.flow.valflow);
}

extern "C" std::size_t xlfn_probe_xloper12_flow_row_offset() {
    return offsetof(decltype(((XLOPER12*)nullptr)->val.flow), rw);
}

extern "C" std::size_t xlfn_probe_xloper12_flow_column_offset() {
    return offsetof(decltype(((XLOPER12*)nullptr)->val.flow), col);
}

extern "C" std::size_t xlfn_probe_xloper12_flow_type_offset() {
    return offsetof(decltype(((XLOPER12*)nullptr)->val.flow), xlflow);
}

extern "C" std::size_t xlfn_probe_xloper12_flow_level_size() {
    return sizeof(((XLOPER12*)nullptr)->val.flow.valflow.level);
}

extern "C" std::size_t xlfn_probe_xloper12_flow_toolbar_control_size() {
    return sizeof(((XLOPER12*)nullptr)->val.flow.valflow.tbctrl);
}

extern "C" std::size_t xlfn_probe_xlref12_size() {
    return sizeof(XLREF12);
}

extern "C" int xlfn_probe_xl_async_return() {
    return xlAsyncReturn;
}

extern "C" int xlfn_probe_xl_event_register() {
    return xlEventRegister;
}

extern "C" int xlfn_probe_xlevent_calculation_ended() {
    return xleventCalculationEnded;
}

extern "C" int xlfn_probe_xlevent_calculation_canceled() {
    return xleventCalculationCanceled;
}

extern "C" int xlfn_probe_xlf_register() {
    return xlfRegister;
}

extern "C" int xlfn_probe_xlf_unregister() {
    return xlfUnregister;
}

extern "C" int xlfn_probe_xlbit_dll_free() {
    return xlbitDLLFree;
}
