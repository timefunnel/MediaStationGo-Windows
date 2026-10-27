# Keep CEF's native intermediate files short inside the normal Cargo target
# directory, without requiring a drive alias or relocating build outputs.
if(CMAKE_VERSION VERSION_LESS "4.2")
  message(FATAL_ERROR
    "Windows CEF builds require CMake 4.2 or newer for short intermediate paths. "
    "Install a current CMake and select it with the CMAKE environment variable.")
endif()

set(CMAKE_INTERMEDIATE_DIR_STRATEGY SHORT CACHE STRING
  "Use hashed intermediate directories and object names" FORCE)
# cef-dll-sys sets this to 500 on its command line, above MSVC's path limit.
set(CMAKE_OBJECT_PATH_MAX 240 CACHE STRING
  "Maximum native object path length on Windows" FORCE)
