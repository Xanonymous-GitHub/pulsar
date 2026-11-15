// Extra ESP-IDF headers for the Rust bindings that `esp-idf-sys` generates (see the
// `bindings_header` entry in Cargo.toml). Its own list does not include these.

// esp_mmu_paddr_to_vaddr / esp_mmu_vaddr_to_paddr: where the PSRAM is mapped (src/platform.rs).
#include "esp_mmu_map.h"
