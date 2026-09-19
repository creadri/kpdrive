cmake_minimum_required(VERSION 3.16)
project(kpdriveoverlay LANGUAGES CXX)

find_package(ECM 6.0 REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})
include(KDEInstallDirs)
include(KDECMakeSettings)
include(KDECompilerSettings NO_POLICY_SCOPE)

find_package(Qt6 REQUIRED COMPONENTS Core Network)
find_package(KF6 REQUIRED COMPONENTS CoreAddons KIO)

set(CMAKE_AUTOMOC ON)
kcoreaddons_add_plugin(kpdriveoverlay INSTALL_NAMESPACE "kf6/overlayicon" SOURCES kpdriveoverlay.cpp)
target_link_libraries(kpdriveoverlay KF6::KIOWidgets Qt6::Network)
