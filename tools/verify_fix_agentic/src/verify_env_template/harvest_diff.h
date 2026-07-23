// Helpers for C-vs-Rust differential comparison. Extend or replace as the
// target API needs — this is a starting point, not a fixed contract.
#ifndef HARVEST_DIFF_H_
#define HARVEST_DIFF_H_

#include <cerrno>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <optional>
#include <sstream>
#include <string>
#include <vector>

namespace harvest {

// --- Output buffers ---------------------------------------------------------

// Fill pattern for output buffers. Initializing dst to a fixed non-zero pattern
// before each call makes it visible when one side writes a different range or
// leaves bytes untouched (a zero fill hides both, because real output contains
// zeros too).
inline constexpr uint8_t kFillByte = 0xA5;

inline std::vector<uint8_t> FilledBuffer(size_t size) {
  return std::vector<uint8_t>(size, kFillByte);
}

// --- Diffing ----------------------------------------------------------------

// Index of the first byte that differs between two buffers, or std::nullopt if
// they are byte-identical over the compared length. Differing lengths count as
// a difference at the first out-of-range index.
inline std::optional<size_t> FirstDifference(const std::vector<uint8_t>& a,
                                             const std::vector<uint8_t>& b) {
  const size_t n = a.size() < b.size() ? a.size() : b.size();
  for (size_t i = 0; i < n; ++i) {
    if (a[i] != b[i]) {
      return i;
    }
  }
  if (a.size() != b.size()) {
    return n;
  }
  return std::nullopt;
}

// Hex dump of up to `max_bytes` bytes, e.g. "13 bytes: 1b 65 01 00 ... (+5)".
inline std::string HexDump(const std::vector<uint8_t>& data,
                           size_t max_bytes = 64) {
  std::ostringstream os;
  os << data.size() << " bytes:";
  const size_t shown = data.size() < max_bytes ? data.size() : max_bytes;
  char buf[4];
  for (size_t i = 0; i < shown; ++i) {
    std::snprintf(buf, sizeof(buf), " %02x", data[i]);
    os << buf;
  }
  if (data.size() > shown) {
    os << " ... (+" << (data.size() - shown) << ")";
  }
  return os.str();
}

// Streamable comparison detail for use as an EXPECT_EQ message, e.g.
//   EXPECT_EQ(rust, c) << harvest::Explain(c, rust);
// Reports the first differing offset and a hex dump of each side.
inline std::string Explain(const std::vector<uint8_t>& c_out,
                           const std::vector<uint8_t>& rust_out) {
  std::ostringstream os;
  const auto diff = FirstDifference(c_out, rust_out);
  if (diff.has_value()) {
    os << "first difference at byte " << *diff << "\n";
  } else {
    os << "buffers are byte-identical\n";
  }
  os << "  C:    " << HexDump(c_out) << "\n";
  os << "  Rust: " << HexDump(rust_out);
  return os.str();
}

// --- errno capture ----------------------------------------------------------

// Clears errno, runs `call`, and returns (result, errno-after). Use to compare
// the errno each side sets, which is otherwise easy to read after it has been
// clobbered by intervening code.
template <typename Fn>
auto WithErrno(Fn&& call) -> std::pair<decltype(call()), int> {
  errno = 0;
  auto result = call();
  return {result, errno};
}

// --- Normalized observation -------------------------------------------------

// A normalized view of one call's observable result, suitable for EXPECT_EQ.
// Compare only fields that are semantically meaningful — drop pointer values,
// padding, timestamps, and other nondeterministic data before building this.
// Extend with whatever the API actually returns (output length, struct fields,
// errno, ...).
struct Observation {
  int return_value = 0;
  std::vector<uint8_t> output;  // only the meaningful prefix, sized by the call

  bool operator==(const Observation& other) const {
    return return_value == other.return_value && output == other.output;
  }
};

inline std::string Explain(const Observation& c, const Observation& rust) {
  std::ostringstream os;
  os << "return: C=" << c.return_value << " Rust=" << rust.return_value << "\n";
  os << Explain(c.output, rust.output);
  return os.str();
}

}  // namespace harvest

#endif  // HARVEST_DIFF_H_
