#include <windows.h>
#include <XLCALL.H>

// This deliberately uses the SDK's MdCallBack12-compatible declaration, not a
// Rust mirror. Calling it through excel12v detects argument-order drift across
// the C++/Rust ABI boundary.
extern "C" int PASCAL xlfn_callback_probe(
    int xlfn,
    int count,
    LPXLOPER12* args,
    LPXLOPER12 result) {
    if (xlfn != 0x1234 || count != 1 || args == nullptr ||
        args[0] == nullptr || result == nullptr) {
        return 32;  // xlretFailed
    }
    result->val.w = xlfn;
    result->xltype = xltypeInt;
    return 0;  // xlretSuccess
}
