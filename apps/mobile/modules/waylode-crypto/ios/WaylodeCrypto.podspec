require 'json'

package = JSON.parse(File.read(File.join(__dir__, '..', '..', '..', 'package.json')))

Pod::Spec.new do |s|
  s.name           = 'WaylodeCrypto'
  s.version        = package['version']
  s.summary        = 'Waylode native hardware P-256 + Rust UniFFI crypto and approval confirmation sheet'
  s.description    = 'Secure Enclave P-256 approval signing, Keychain Ed25519/X25519 identity isolation, native full-bytes approval sheet, and src-tauri/crates/waylode-protocol XCFramework bridge.'
  s.license        = 'MIT OR Apache-2.0'
  s.author         = 'Waylode'
  s.homepage       = 'https://github.com/bharathvbcr/Waylode'
  s.platforms      = { :ios => '16.0' }
  s.swift_version  = '5.9'
  s.source         = { :git => 'https://github.com/bharathvbcr/Waylode.git', :tag => s.version.to_s }
  s.static_framework = true

  s.dependency 'ExpoModulesCore'

  s.source_files = '**/*.{h,m,swift}'
  s.vendored_frameworks = 'Frameworks/WaylodeProtocol.xcframework'
  s.frameworks = 'Security', 'LocalAuthentication', 'CryptoKit', 'UIKit'
end
