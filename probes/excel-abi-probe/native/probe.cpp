#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif

#ifndef NOMINMAX
#define NOMINMAX
#endif

#include <Windows.h>
#include <XLCALL.H>

#include <cstddef>

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
