// Loads the translated Rust cdylib as a black box and resolves its C-ABI
// exports. RTLD_LOCAL keeps the Rust symbols from colliding with the C
// reference of the same name that is linked statically into this binary.
//
// The path comes from the RUST_LIB_PATH environment variable (the harness sets
// it; when running by hand, export it to the built .so).
#ifndef HARVEST_RUST_LIB_H_
#define HARVEST_RUST_LIB_H_

#include <dlfcn.h>

#include <cstdlib>

#include "gtest/gtest.h"

namespace harvest {

// Opens the Rust cdylib once per process and hands out symbol lookups.
class RustLib {
 public:
  static RustLib& Get() {
    static RustLib lib;
    return lib;
  }

  // Resolves a function symbol; fails the current test if it is missing.
  template <typename Fn>
  Fn Sym(const char* name) {
    if (handle_ == nullptr) {
      ADD_FAILURE() << "Rust library not loaded (RUST_LIB_PATH unset or dlopen "
                       "failed)";
      return nullptr;
    }
    void* sym = dlsym(handle_, name);
    if (sym == nullptr) {
      ADD_FAILURE() << "symbol not found in Rust library: " << name
                    << " (is it exported with #[no_mangle] extern \"C\"?)";
      return nullptr;
    }
    return reinterpret_cast<Fn>(sym);
  }

  bool ok() const { return handle_ != nullptr; }

 private:
  RustLib() {
    const char* path = std::getenv("RUST_LIB_PATH");
    if (path == nullptr) {
      return;
    }
    handle_ = dlopen(path, RTLD_NOW | RTLD_LOCAL);
  }

  void* handle_ = nullptr;
};

}  // namespace harvest

#endif  // HARVEST_RUST_LIB_H_
