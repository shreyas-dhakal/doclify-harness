class Doclify < Formula
  desc "Capture coding agent sessions and turn them into reviewed documentation"
  homepage "https://github.com/shreyas-dhakal/doclify-harness"
  head "https://github.com/shreyas-dhakal/doclify-harness.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "build", "--release", "--locked", "-p", "doclify"
    bin.install "target/release/doclify"
  end

  test do
    assert_match "doclify", shell_output("#{bin}/doclify --help")
  end
end
