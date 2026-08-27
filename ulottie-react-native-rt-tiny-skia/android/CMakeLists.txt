cmake_minimum_required(VERSION 3.22)
project(ulottierttinyskia)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(ReactAndroid REQUIRED CONFIG)
find_package(ulottie-react-native REQUIRED CONFIG)

# Built by scripts/build-rust.sh android (cargo, profile `rt`).
add_library(rust_ulottie_rt STATIC IMPORTED)
set_target_properties(rust_ulottie_rt PROPERTIES
  IMPORTED_LOCATION ${CMAKE_CURRENT_SOURCE_DIR}/rust/${ANDROID_ABI}/libulottie_rt.a)

add_library(ulottierttinyskia SHARED src/main/cpp/UlottieRtAdapter.cpp)
# The C ABI header lives with the Rust crate (workspace layout; these are
# private workspace packages).
target_include_directories(ulottierttinyskia PRIVATE
  ${CMAKE_CURRENT_SOURCE_DIR}/../../ulottie-rt/include)
target_compile_options(ulottierttinyskia PRIVATE -fexceptions)

target_link_libraries(ulottierttinyskia
  rust_ulottie_rt
  ulottie-react-native::ulottiertshared
  ReactAndroid::jsi
  android
  jnigraphics
  log
  m)
# --exclude-libs,ALL keeps every symbol of the Rust archive out of the dynamic
# table: the ThorVG backend .so carries the same crate built with the other
# feature, and the hidden visibility is what keeps the two from interposing.
target_link_options(ulottierttinyskia PRIVATE
  "-Wl,--exclude-libs,ALL"
  "-Wl,-z,max-page-size=16384")
