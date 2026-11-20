cmake_minimum_required(VERSION 3.20)
project(flipperaki_c_tests C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)

include(FetchContent)
# SOURCE_SUBDIR points at nothing so only the sources are fetched; we compile them below.
FetchContent_Declare(unity
    URL https://github.com/ThrowTheSwitch/Unity/archive/b6763fbd9cedfacaa89e2ad9fd00d615a234e355.tar.gz
    URL_HASH SHA256=50edf40d4c433cfb7935a3de0d8a3c1f497576fdd472acf48bf84507651025fc
    SOURCE_SUBDIR none)
FetchContent_Declare(nanopb
    URL https://github.com/nanopb/nanopb/archive/refs/tags/0.4.8.tar.gz
    URL_HASH SHA256=3f78bf63722a810edb6da5ab5f0e76c7db13a961c2aad4ab49296e3095d0d830
    SOURCE_SUBDIR none)
FetchContent_MakeAvailable(unity nanopb)

set(REPO ${CMAKE_CURRENT_SOURCE_DIR}/../..)

add_library(unity STATIC ${unity_SOURCE_DIR}/src/unity.c)
target_include_directories(unity SYSTEM PUBLIC ${unity_SOURCE_DIR}/src)

# nanopb runtime for host tests only; on device the FAP links the SDK's exports.
add_library(nanopb STATIC
    ${nanopb_SOURCE_DIR}/pb_common.c
    ${nanopb_SOURCE_DIR}/pb_decode.c
    ${nanopb_SOURCE_DIR}/pb_encode.c)
target_include_directories(nanopb SYSTEM PUBLIC ${nanopb_SOURCE_DIR})

add_library(buddy_core STATIC
    ${REPO}/flipper/buddy_core.c
    ${REPO}/flipper/buddy_protocol.c
    ${REPO}/protocol/generated/buddy.pb.c)
target_include_directories(buddy_core PUBLIC ${REPO}/flipper ${REPO}/protocol/generated)
target_link_libraries(buddy_core PUBLIC nanopb)
target_compile_options(buddy_core PRIVATE -Wall -Wextra -Werror)

add_executable(test_buddy_core test_buddy_core.c)
target_link_libraries(test_buddy_core PRIVATE buddy_core unity)
target_compile_options(test_buddy_core PRIVATE -Wall -Wextra -Werror)
target_compile_definitions(test_buddy_core PRIVATE FIXTURES_DIR="${REPO}/protocol/fixtures")

add_executable(buddy_fixtures buddy_fixtures.c)
target_link_libraries(buddy_fixtures PRIVATE buddy_core)
target_compile_options(buddy_fixtures PRIVATE -Wall -Wextra -Werror)

enable_testing()
add_test(NAME buddy_core COMMAND test_buddy_core)
# Committed cross-language fixtures must equal what the C encoder produces.
add_test(NAME fixtures_current COMMAND buddy_fixtures --check ${REPO}/protocol/fixtures/c)
