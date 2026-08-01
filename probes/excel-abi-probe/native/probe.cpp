#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif

#ifndef NOMINMAX
#define NOMINMAX
#endif

#include <Windows.h>
#include <XLCALL.H>

#include <cstddef>
#include <iostream>

int main() {
    std::cout << "XLOPER12.size=" << sizeof(XLOPER12) << '\n';
    std::cout << "XLOPER12.align=" << alignof(XLOPER12) << '\n';
    std::cout << "XLOPER12.xltype.offset=" << offsetof(XLOPER12, xltype) << '\n';

    std::cout << "XLOPER12Array.size=" << sizeof(((XLOPER12*)nullptr)->val.array) << '\n';

    std::cout << "XLOPER12SRef.size=" << sizeof(((XLOPER12*)nullptr)->val.sref) << '\n';

    std::cout << "IDSHEET.size=" << sizeof(IDSHEET) << '\n';

    std::cout << "XLOPER12MRef.size=" << sizeof(((XLOPER12*)nullptr)->val.mref) << '\n';

    std::cout << "XLOPER12MRef.idSheet.offset="
              << offsetof(decltype(((XLOPER12*)nullptr)->val.mref), idSheet) << '\n';

    std::cout << "XLOPER12Flow.size=" << sizeof(((XLOPER12*)nullptr)->val.flow) << '\n';

    std::cout << "XLOPER12FlowValue.size=" << sizeof(((XLOPER12*)nullptr)->val.flow.valflow)
              << '\n';

    std::cout << "XLOPER12Flow.rw.offset=" << offsetof(decltype(((XLOPER12*)nullptr)->val.flow), rw)
              << '\n';

    std::cout << "XLOPER12Flow.col.offset="
              << offsetof(decltype(((XLOPER12*)nullptr)->val.flow), col) << '\n';

    std::cout << "XLOPER12Flow.xlflow.offset="
              << offsetof(decltype(((XLOPER12*)nullptr)->val.flow), xlflow) << '\n';

    std::cout << "XLOPER12FlowValue.level.size="
              << sizeof(((XLOPER12*)nullptr)->val.flow.valflow.level) << '\n';

    std::cout << "XLOPER12FlowValue.tbctrl.size="
              << sizeof(((XLOPER12*)nullptr)->val.flow.valflow.tbctrl) << '\n';

    std::cout << "XLREF12.size=" << sizeof(XLREF12) << '\n';
    std::cout << "xlAsyncReturn=" << xlAsyncReturn << '\n';
    std::cout << "xlEventRegister=" << xlEventRegister << '\n';
    std::cout << "xleventCalculationEnded=" << xleventCalculationEnded << '\n';
    std::cout << "xleventCalculationCanceled=" << xleventCalculationCanceled << '\n';
    std::cout << "xlfRegister=" << xlfRegister << '\n';
    std::cout << "xlfUnregister=" << xlfUnregister << '\n';
    std::cout << "xlbitDLLFree=" << xlbitDLLFree << '\n';

    return 0;
}
