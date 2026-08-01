cmake_minimum_required(VERSION 3.20)
project(ExcelAbiProbe LANGUAGES CXX)

if(NOT DEFINED XLFN_SDK_INCLUDE)
    message(FATAL_ERROR "Set XLFN_SDK_INCLUDE to the directory containing XLCALL.H")
endif()

add_executable(excel_abi_probe probe.cpp)
target_include_directories(excel_abi_probe PRIVATE "${XLFN_SDK_INCLUDE}")
target_compile_features(excel_abi_probe PRIVATE cxx_std_17)
if(MSVC)
    target_compile_options(excel_abi_probe PRIVATE /W4 /WX /permissive-)
endif()

