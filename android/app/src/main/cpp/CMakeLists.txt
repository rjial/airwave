cmake_minimum_required(VERSION 3.18)
project(airwave_native C CXX)

# OpenSL ES path (always built with the NDK).
add_library(airwave_opensl SHARED opensl_out.c)
target_link_libraries(airwave_opensl OpenSLES log)

# Oboe path. Oboe ships as a prefab package pulled in by the Gradle dependency.
find_package(oboe REQUIRED CONFIG)
add_library(airwave_oboe SHARED oboe_out.cpp)
target_link_libraries(airwave_oboe oboe::oboe log)
