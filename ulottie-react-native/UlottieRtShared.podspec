require "json"

package = JSON.parse(File.read(File.join(__dir__, "package.json")))

Pod::Spec.new do |s|
  s.name         = "UlottieRtShared"
  s.version      = package["version"]
  s.summary      = "ulottie rt shared native core (JSI api + view registry)"
  s.homepage     = "https://github.com/cometkim/ulottie"
  s.license      = "MIT"
  s.authors      = { "ulottie" => "noreply@ulottie.dev" }
  s.platforms    = { :ios => "16.4" }
  s.source       = { :git => "https://github.com/cometkim/ulottie.git", :tag => s.version.to_s }

  # The backend-agnostic half of the rt native code: the `global.UlottieRtApi`
  # JSI host object, the nativeId -> view registry it dispatches through, and
  # UlottieRtBaseView — the whole surface/buffer/blit view, parameterized by
  # a backend's five-function C ABI table (UlottieRtBackend.h). Each
  # rasterizer pod (UlottieRtTinySkia, UlottieRtThorvg) contributes only its
  # codegen Fabric descriptor + TurboModule + backend table and depends on
  # this pod, so an app linking both backends gets exactly one registry and
  # one JS binding.
  s.source_files = "cpp/**/*.{h,cpp}", "ios/**/*.{h,mm}"
  s.header_dir   = "UlottieRtShared"

  install_modules_dependencies(s)
end
