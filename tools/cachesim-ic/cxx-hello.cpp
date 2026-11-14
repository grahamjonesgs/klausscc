#include <cstdio>
#include <stdexcept>
#include <string>
#include <vector>
struct D { const char *n; ~D() { std::printf("dtor %s\n", n); } };
[[gnu::noinline]] int thrower(int x) { D d{"thrower"}; if (x > 0) throw std::runtime_error("boom " + std::to_string(x)); return x; }
int main() {
  std::vector<int> v{1, 2, 3};
  try { D d{"main"}; thrower(v[2]); std::puts("FAIL not thrown"); }
  catch (const std::exception &e) { std::printf("caught: %s\n", e.what()); }
  try { throw 42; } catch (int i) { std::printf("int %d\n", i); }
  std::puts("DONE");
}
