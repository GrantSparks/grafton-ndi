"""Query the Windows libclang DLL from the directory bindgen will search."""
import ctypes
import os
from pathlib import Path


class CXString(ctypes.Structure):
    _fields_ = [('data', ctypes.c_void_p), ('private_flags', ctypes.c_uint)]


def main():
    directory = Path(os.environ['LIBCLANG_PATH'])
    with os.add_dll_directory(str(directory)):
        clang = ctypes.CDLL(str(directory / 'libclang.dll'))
        clang.clang_getClangVersion.argtypes = []
        clang.clang_getClangVersion.restype = CXString
        clang.clang_getCString.argtypes = [CXString]
        clang.clang_getCString.restype = ctypes.c_char_p
        clang.clang_disposeString.argtypes = [CXString]
        clang.clang_disposeString.restype = None
        version = clang.clang_getClangVersion()
        try:
            print(clang.clang_getCString(version).decode('utf-8'))
        finally:
            clang.clang_disposeString(version)


if __name__ == '__main__':
    main()
