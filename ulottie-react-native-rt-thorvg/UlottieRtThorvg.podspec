require "json"

package = JSON.parse(File.read(File.join(__dir__, "package.json")))

Pod::Spec.new do |s|
  s.name         = "UlottieRtThorvg"
  s.version      = package["version"]
  s.summary      = "ulottie native rasterizer target (ThorVG SW engine)"
  s.homepage     = "https://github.com/cometkim/ulottie"
  s.license      = "MIT"
  s.authors      = { "ulottie" => "noreply@ulottie.dev" }
  s.platforms    = { :ios => "16.4" }
  s.source       = { :git => "https://github.com/cometkim/ulottie.git", :tag => s.version.to_s }

  s.source_files = "ios/**/*.{h,m,mm,cpp}"
  s.private_header_files = "ios/**/*.h"

  # JSI api + view registry shared with the other rasterizer pods; lives in
  # the base `ulottie-react-native` package (UlottieRtShared.podspec).
  s.dependency "UlottieRtShared"

  # Built by scripts/build-rust.sh (cargo, profile `rt`, feature `thorvg`).
  # ThorVG v1.1.1 is compiled from source by ulottie-rt/build.rs and bundled
  # into this archive, which is why the pod links libc++ (ThorVG is C++) and
  # vendors no separate ThorVG binary. Simulator-only for now; the device
  # slice joins as an XCFramework when it lands.
  s.vendored_libraries = "ios/rust/libulottie_rt_tvg.a"
  s.libraries = "c++"

  s.pod_target_xcconfig = {
    "HEADER_SEARCH_PATHS" => "\"$(PODS_TARGET_SRCROOT)/ios/rust\"",
  }

  install_modules_dependencies(s)
end
