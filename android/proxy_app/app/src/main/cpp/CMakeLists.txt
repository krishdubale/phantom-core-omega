cmake_minimum_required(VERSION 3.18.1)
project("phantom-proxy" LANGUAGES CXX)

add_library(phantom-proxy SHARED proxy_native.cpp)

target_compile_options(phantom-proxy PRIVATE
    -Wall -Wextra -O2 -std=c++17
)

find_library(log-lib log)
find_library(android-lib android)

target_link_libraries(phantom-proxy
    ${log-lib}
    ${android-lib}
)
