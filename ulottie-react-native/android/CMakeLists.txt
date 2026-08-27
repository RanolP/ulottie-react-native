cmake_minimum_required(VERSION 3.22)
project(ulottiertshared)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(ReactAndroid REQUIRED CONFIG)

add_library(ulottiertshared SHARED
  ../cpp/UlottieRtApi.cpp
  ../cpp/UlottieRtRegistry.cpp
  src/main/cpp/UlottieRtAndroidAdapter.cpp)

# Consumers include <UlottieRtShared/...>; in-tree the parent of cpp/ serves
# the same layout the prefab headers dir replicates for out-of-tree builds.
# ../cpp lets the Android adapter (which lives outside cpp/ because it needs
# <jni.h>) reach the shared headers by their bare names.
target_include_directories(ulottiertshared PRIVATE .. ../cpp)
# jsi throws JSError; the sources need exceptions even though RN builds
# without RTTI.
target_compile_options(ulottiertshared PRIVATE -fexceptions -fvisibility=default)

target_link_libraries(ulottiertshared
  ReactAndroid::jsi
  android
  jnigraphics
  log)
target_link_options(ulottiertshared PRIVATE "-Wl,-z,max-page-size=16384")
