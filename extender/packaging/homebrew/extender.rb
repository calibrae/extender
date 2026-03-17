class Extender < Formula
  desc "Share USB devices over the network"
  homepage "https://github.com/calibrae/extender"
  url "https://github.com/calibrae/extender/archive/refs/tags/v0.1.0.tar.gz"
  sha256 ""
  license any_of: ["MIT", "Apache-2.0"]

  depends_on "rust" => :build
  depends_on "libusb"

  def install
    cd "extender" do
      system "cargo", "install", *std_cargo_args
    end
  end

  service do
    run [opt_bin/"extender", "daemon"]
    keep_alive true
    log_path var/"log/extender.log"
    error_log_path var/"log/extender.log"
  end

  test do
    assert_match "extender #{version}", shell_output("#{bin}/extender version")
  end
end
