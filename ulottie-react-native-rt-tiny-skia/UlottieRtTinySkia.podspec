require "json"

package = JSON.parse(File.read(File.join(__dir__, "package.json")))

Pod::Spec.new do |s|
  s.name         = "UlottieRtTinySkia"
  s.version      = package["version"]
  s.summary      = "ulottie native rasterizer target (tiny-skia)"
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

  # Built by scripts/build-rust.sh (cargo, profile `rt`). Simulator-only for
  # now; the device slice joins as an XCFramework when it lands.
  s.vendored_libraries = "ios/rust/libulottie_rt.a"

  s.pod_target_xcconfig = {
    "HEADER_SEARCH_PATHS" => "\"$(PODS_TARGET_SRCROOT)/ios/rust\"",
  }

  install_modules_dependencies(s)
end
