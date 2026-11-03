#include <windows.h>
#include <XLCALL.H>

// XLCALL.H embeds these records anonymously inside XLOPER12. Give bindgen
// stable names for the exact member types without duplicating their layout.
using XLOPER12SRef = decltype(((XLOPER12*)nullptr)->val.sref);
using XLOPER12MRef = decltype(((XLOPER12*)nullptr)->val.mref);
using XLOPER12Array = decltype(((XLOPER12*)nullptr)->val.array);
using XLOPER12FlowValue = decltype(((XLOPER12*)nullptr)->val.flow.valflow);
using XLOPER12Flow = decltype(((XLOPER12*)nullptr)->val.flow);
