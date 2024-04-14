use std::{
	env, fs,
	path::{Path, PathBuf}
};

const ORT_ENV_SYSTEM_LIB_LOCATION: &str = "ORT_LIB_LOCATION";
const ORT_ENV_SYSTEM_LIB_PROFILE: &str = "ORT_LIB_PROFILE";

#[path = "src/internal/dirs.rs"]
mod dirs;
use self::dirs::cache_dir;

#[cfg(feature = "download-binaries")]
fn fetch_file(source_url: &str) -> Vec<u8> {
	let resp = ureq::AgentBuilder::new()
		.try_proxy_from_env(true)
		.build()
		.get(source_url)
		.timeout(std::time::Duration::from_secs(1800))
		.call()
		.unwrap_or_else(|err| panic!("Failed to GET `{source_url}`: {err}"));

	let len = resp
		.header("Content-Length")
		.and_then(|s| s.parse::<usize>().ok())
		.expect("Content-Length header should be present on archive response");
	let mut reader = resp.into_reader();
	let mut buffer = Vec::new();
	reader
		.read_to_end(&mut buffer)
		.unwrap_or_else(|err| panic!("Failed to download from `{source_url}`: {err}"));
	assert_eq!(buffer.len(), len);
	buffer
}

#[cfg(feature = "download-binaries")]
fn hex_str_to_bytes(c: impl AsRef<[u8]>) -> Vec<u8> {
	fn nibble(c: u8) -> u8 {
		match c {
			b'A'..=b'F' => c - b'A' + 10,
			b'a'..=b'f' => c - b'a' + 10,
			b'0'..=b'9' => c - b'0',
			_ => panic!()
		}
	}

	c.as_ref().chunks(2).map(|n| nibble(n[0]) << 4 | nibble(n[1])).collect()
}

#[cfg(feature = "download-binaries")]
fn verify_file(buf: &[u8], hash: impl AsRef<[u8]>) -> bool {
	use sha2::Digest;
	sha2::Sha256::digest(buf)[..] == hex_str_to_bytes(hash)
}

#[cfg(feature = "download-binaries")]
fn extract_tgz(buf: &[u8], output: &Path) {
	let buf: std::io::BufReader<&[u8]> = std::io::BufReader::new(buf);
	let tar = flate2::read::GzDecoder::new(buf);
	let mut archive = tar::Archive::new(tar);
	archive.unpack(output).expect("Failed to extract .tgz file");
}

#[cfg(feature = "copy-dylibs")]
fn copy_libraries(lib_dir: &Path, out_dir: &Path) {
	// get the target directory - we need to place the dlls next to the executable so they can be properly loaded by windows
	let out_dir = out_dir.ancestors().nth(3).unwrap();
	for out_dir in [out_dir.to_path_buf(), out_dir.join("examples"), out_dir.join("deps")] {
		#[cfg(windows)]
		let mut copy_fallback = false;
		#[cfg(not(windows))]
		let copy_fallback = false;

		let lib_files = std::fs::read_dir(lib_dir).unwrap_or_else(|_| panic!("Failed to read contents of `{}` (does it exist?)", lib_dir.display()));
		for lib_file in lib_files.filter(|e| {
			e.as_ref().ok().map_or(false, |e| {
				e.file_type().map_or(false, |e| !e.is_dir()) && [".dll", ".so", ".dylib"].into_iter().any(|v| e.path().to_string_lossy().contains(v))
			})
		}) {
			let lib_file = lib_file.unwrap();
			let lib_path = lib_file.path();
			let lib_name = lib_path.file_name().unwrap();
			let out_path = out_dir.join(lib_name);
			if !out_path.exists() {
				if out_path.is_symlink() {
					fs::remove_file(&out_path).unwrap();
				}
				#[cfg(windows)]
				if std::os::windows::fs::symlink_file(&lib_path, &out_path).is_err() {
					copy_fallback = true;
					std::fs::copy(&lib_path, &out_path).unwrap();
				}
				#[cfg(unix)]
				std::os::unix::fs::symlink(&lib_path, &out_path).unwrap();
			}
			if !copy_fallback {
				println!("cargo:rerun-if-changed={}", out_path.to_str().unwrap());
			}
		}

		#[cfg(target_os = "linux")]
		{
			let main_dy = lib_dir.join("libonnxruntime.so");
			let versioned_dy = out_dir.join("libonnxruntime.so.1.17.1");
			if main_dy.exists() && !versioned_dy.exists() {
				if versioned_dy.is_symlink() {
					fs::remove_file(&versioned_dy).unwrap();
				}
				std::os::unix::fs::symlink(main_dy, versioned_dy).unwrap();
			}
		}

		// If we had to fallback to copying files on Windows, break early to avoid copying to 3 different directories
		if copy_fallback {
			break;
		}
	}
}

fn add_search_dir<P: AsRef<Path>>(base: P) {
	let base = base.as_ref();
	if base.join("Release").is_dir() {
		println!("cargo:rustc-link-search=native={}", base.join("Release").display());
	} else if base.join("Debug").is_dir() {
		println!("cargo:rustc-link-search=native={}", base.join("Debug").display());
	} else {
		println!("cargo:rustc-link-search=native={}", base.display());
	}
}

fn static_link_prerequisites(using_pyke_libs: bool) {
	let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
	if target_os == "macos" || target_os == "ios" {
		println!("cargo:rustc-link-lib=c++");
		println!("cargo:rustc-link-lib=framework=Foundation");
	} else if target_os == "linux" || target_os == "android" {
		println!("cargo:rustc-link-lib=stdc++");
	} else if target_os == "windows" && (using_pyke_libs || cfg!(feature = "directml")) {
		println!("cargo:rustc-link-lib=dxguid");
		println!("cargo:rustc-link-lib=DXCORE");
		println!("cargo:rustc-link-lib=DXGI");
		println!("cargo:rustc-link-lib=D3D12");
		println!("cargo:rustc-link-lib=DirectML");
	}
}

fn prepare_libort_dir() -> (PathBuf, bool) {
	if let Ok(lib_dir) = env::var(ORT_ENV_SYSTEM_LIB_LOCATION) {
		let lib_dir = PathBuf::from(lib_dir);

		let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap().to_lowercase();
		let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap().to_lowercase();
		let platform_format_lib = |a: &str| {
			if target_os.contains("windows") { format!("{}.lib", a) } else { format!("lib{}.a", a) }
		};

		let mut profile = env::var(ORT_ENV_SYSTEM_LIB_PROFILE).unwrap_or_default();
		if profile.is_empty() {
			for i in ["Release", "RelWithDebInfo", "MinSizeRel", "Debug"] {
				if lib_dir.join(i).exists() && lib_dir.join(i).join(platform_format_lib("onnxruntime_common")).exists() {
					profile = String::from(i);
					break;
				}
			}
		}

		add_search_dir(&lib_dir);

		let mut needs_link = true;
		if lib_dir.join(platform_format_lib("onnxruntime")).exists() {
			println!("cargo:rustc-link-lib=static=onnxruntime");
			needs_link = false;
		} else {
			#[allow(clippy::type_complexity)]
			let static_configs: Vec<(PathBuf, PathBuf, PathBuf, Box<dyn Fn(PathBuf, &String) -> PathBuf>)> = vec![
				(lib_dir.join(&profile), lib_dir.join("lib"), lib_dir.join("_deps"), Box::new(|p: PathBuf, profile| p.join(profile))),
				(lib_dir.join(&profile), lib_dir.join("lib"), lib_dir.join(&profile).join("_deps"), Box::new(|p: PathBuf, _| p)),
				(lib_dir.clone(), lib_dir.join("lib"), lib_dir.parent().unwrap().join("_deps"), Box::new(|p: PathBuf, _| p)),
				(lib_dir.join("onnxruntime"), lib_dir.join("onnxruntime").join("lib"), lib_dir.join("_deps"), Box::new(|p: PathBuf, _| p)),
			];
			for (lib_dir, extension_lib_dir, external_lib_dir, transform_dep) in static_configs {
				if lib_dir.join(platform_format_lib("onnxruntime_common")).exists() && external_lib_dir.exists() {
					add_search_dir(&lib_dir);

					for lib in &["common", "flatbuffers", "framework", "graph", "mlas", "optimizer", "providers", "session", "util"] {
						let lib_path = lib_dir.join(platform_format_lib(&format!("onnxruntime_{lib}")));
						// sanity check, just make sure the library exists before we try to link to it
						if lib_path.exists() {
							println!("cargo:rustc-link-lib=static=onnxruntime_{lib}");
						} else {
							panic!("[ort] unable to find ONNX Runtime library: {}", lib_path.display());
						}
					}

					if extension_lib_dir.exists() && extension_lib_dir.join(platform_format_lib("ortcustomops")).exists() {
						add_search_dir(&extension_lib_dir);
						println!("cargo:rustc-link-lib=static=ortcustomops");
						println!("cargo:rustc-link-lib=static=ocos_operators");
						println!("cargo:rustc-link-lib=static=noexcep_operators");
					}

					if target_arch == "wasm32" {
						for lib in &["webassembly", "providers_js"] {
							let lib_path = lib_dir.join(platform_format_lib(&format!("onnxruntime_{lib}")));
							if lib_path.exists() {
								println!("cargo:rustc-link-lib=static=onnxruntime_{lib}");
							}
						}
					}

					let protobuf_build = transform_dep(external_lib_dir.join("protobuf-build"), &profile);
					add_search_dir(&protobuf_build);
					for lib in ["protobuf-lited", "protobuf-lite", "protobuf"] {
						if target_os.contains("windows") && protobuf_build.join(platform_format_lib(&format!("lib{lib}"))).exists() {
							println!("cargo:rustc-link-lib=static=lib{lib}")
						} else if protobuf_build.join(platform_format_lib(lib)).exists() {
							println!("cargo:rustc-link-lib=static={lib}");
						}
					}

					add_search_dir(transform_dep(external_lib_dir.join("onnx-build"), &profile));
					println!("cargo:rustc-link-lib=static=onnx");
					println!("cargo:rustc-link-lib=static=onnx_proto");

					add_search_dir(transform_dep(external_lib_dir.join("google_nsync-build"), &profile));
					println!("cargo:rustc-link-lib=static=nsync_cpp");

					if target_arch != "wasm32" {
						add_search_dir(transform_dep(external_lib_dir.join("pytorch_cpuinfo-build"), &profile));
						let clog_path = transform_dep(external_lib_dir.join("pytorch_cpuinfo-build").join("deps").join("clog"), &profile);
						if clog_path.exists() {
							add_search_dir(clog_path);
						} else {
							add_search_dir(transform_dep(external_lib_dir.join("pytorch_clog-build"), &profile));
						}
						println!("cargo:rustc-link-lib=static=cpuinfo");
						println!("cargo:rustc-link-lib=static=clog");
					}

					add_search_dir(transform_dep(external_lib_dir.join("re2-build"), &profile));
					println!("cargo:rustc-link-lib=static=re2");

					add_search_dir(transform_dep(external_lib_dir.join("abseil_cpp-build").join("absl").join("base"), &profile));
					println!("cargo:rustc-link-lib=static=absl_base");
					println!("cargo:rustc-link-lib=static=absl_throw_delegate");
					add_search_dir(transform_dep(external_lib_dir.join("abseil_cpp-build").join("absl").join("hash"), &profile));
					println!("cargo:rustc-link-lib=static=absl_hash");
					println!("cargo:rustc-link-lib=static=absl_city");
					println!("cargo:rustc-link-lib=static=absl_low_level_hash");
					add_search_dir(transform_dep(external_lib_dir.join("abseil_cpp-build").join("absl").join("container"), &profile));
					println!("cargo:rustc-link-lib=static=absl_raw_hash_set");

					if cfg!(feature = "coreml") && (target_os == "macos" || target_os == "ios") {
						println!("cargo:rustc-link-lib=framework=CoreML");
						println!("cargo:rustc-link-lib=coreml_proto");
						println!("cargo:rustc-link-lib=onnxruntime_providers_coreml");
					}

					// #[cfg(feature = "rocm")]
					// println!("cargo:rustc-link-lib=onnxruntime_providers_rocm");

					needs_link = false;
					break;
				}
			}
			if needs_link {
				// none of the static link patterns matched, we might be trying to dynamic link so copy dylibs if requested
				#[cfg(feature = "copy-dylibs")]
				{
					let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
					if lib_dir.join("lib").is_dir() {
						copy_libraries(&lib_dir.join("lib"), &out_dir);
					} else if lib_dir.join(&profile).is_dir() {
						copy_libraries(&lib_dir.join(profile), &out_dir);
					}
				}
			}
		}

		(lib_dir, needs_link)
	} else {
		#[cfg(feature = "download-binaries")]
		{
			#[cfg(any(
				feature = "tensorrt",
				feature = "openvino",
				feature = "onednn",
				feature = "nnapi",
				feature = "coreml",
				feature = "xnnpack",
				feature = "rocm",
				feature = "acl",
				feature = "armnn",
				feature = "tvm",
				feature = "migraphx",
				feature = "rknpu",
				feature = "vitis",
				feature = "cann",
				feature = "qnn"
			))]
			compile_error!("unsupported EP");

			let target = env::var("TARGET").unwrap().to_string();
			let (prebuilt_url, prebuilt_hash) = match target.as_str() {
				"aarch64-apple-darwin" => (
					"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-osx-arm64-1.17.0.tgz",
					"9391BD67F47F911FC9348844A0D8DCC09742E521F677F11D339EBD2D873189FF"
				),
				//"aarch64-pc-windows-msvc" => (
				// 	"https://parcel.pyke.io/v2/delivery/ortrs/packages/msort-binary/1.17.0/ortrs-msort_static-v1.17.0-aarch64-pc-windows-msvc.tgz",
				// 	"27DDC61E1416E3F1BC6137C8365B563F73BA5A6CE8D7008E5CD4E36B4F037FDA"
				//),
				"aarch64-unknown-linux-gnu" => (
					"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-linux-arm64-1.17.0.tgz",
					"F7FE4F8BAA348229AD1CCBF1A1EFE57061E15280E4CFA18EC21508ADE3760EA4"
				),
				//"wasm32-unknown-emscripten" => (
				// 	"https://parcel.pyke.io/v2/delivery/ortrs/packages/msort-binary/1.17.0/ortrs-msort_static-v1.17.0-wasm32-unknown-emscripten.tgz",
				// 	"E1ADBF06922649A59AB9D0459E9D5985B002C3AE830B512B7AED030BDA859C55"
				//),
				"x86_64-apple-darwin" => (
					"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-osx-x86_64-1.17.0.tgz",
					"ADDA1382FD1DBCCA05F93E19F455585D16910F5B621AB2F068BC46B2D5DEB045"
				),
				"x86_64-pc-windows-msvc" => {
					if cfg!(any(feature = "cuda", feature = "directml")) {
						(
							"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-win-x64-gpu-1.17.0.tgz",
							"47603969633BA650704D2A12F12977C14DB780AF13F96AF72EE44D99045F2331"
						)
					} else {
						(
							"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-win-x64-1.17.0.tgz",
							"3DD15FBE4A0A689CA324BBA2319D81C3631537C8243140FA1F93A609DA8E7F10"
						)
					}
				}
				"x86_64-unknown-linux-gnu" => {
					if cfg!(feature = "cuda") {
						(
							"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-linux-x64-gpu-1.17.0.tgz",
							"66FF4B35B9EF8E887959093FE9D17976BE9D5FD4412ED6C4B55D88EE35410281"
						)
					} else {
						(
							"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-linux-x64-1.17.0.tgz",
							"83214CA909838BCF5491E53B2A27A5E8A2F788DC3F4C68820BB0C01ECA2B7558"
						)
					}
				}
				"i686-pc-windows-msvc" => (
					"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-win-x86-1.17.0.tgz",
					"64B2AE803EA270DECF08C2143075262F8A608A8999DE988B2CFA4294470946DC"
				),
				"aarch64-linux-android" => (
					"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-android-arm64-1.17.0.tgz",
					"E5294ED5FF7F3279ECEDB0E9007EFFFC053AB50E6F44AEDB9B32CA6D9257F04F"
				),
				"x86_64-linux-android" => (
					"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-android-x64-1.17.0.tgz",
					"2790DC9E1C5BD3A06E418C34007AE73AE1333E50A71330B30E6DE4653740A342"
				),
				"aarch64-apple-ios" => (
					"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-ios-arm64-1.17.0.tgz",
					"0AE3B6755DCAE66D6F64A467D597CBABF085EE4F8761C3EE9D6944A013209500"
				),
				"aarch64-apple-ios-sim" => (
					"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-ios-sim-arm64-1.17.0.tgz",
					"17C8108ACA3CB8696B10BBDF7BC36C098A03A5667C45D767A01C6E6F375E979A"
				),
				"x86_64-apple-ios" => (
					"https://github.com/VOICEVOX/onnxruntime-builder/releases/download/1.17.0/onnxruntime-ios-sim-x86_64-1.17.0.tgz",
					"A9038F24F7185594E35DA5A5144DDAA2E84D9F8B065272A956D6339F68AF05CD"
				),
				x => panic!("downloaded binaries not available for target {x}\nyou may have to compile ONNX Runtime from source")
			};

			let mut cache_dir = cache_dir()
				.expect("could not determine cache directory")
				.join("dfbin")
				.join(target)
				.join(prebuilt_hash);
			if fs::create_dir_all(&cache_dir).is_err() {
				cache_dir = env::var("OUT_DIR").unwrap().into();
			}

			let ort_extract_dir = prebuilt_url.split('/').last().unwrap().strip_suffix(".tgz").unwrap();
			let lib_dir = cache_dir.join(ort_extract_dir);
			if !lib_dir.exists() {
				let downloaded_file = fetch_file(prebuilt_url);
				assert!(verify_file(&downloaded_file, prebuilt_hash), "hash does not match!");
				extract_tgz(&downloaded_file, &cache_dir);
			}

			static_link_prerequisites(true);

			#[cfg(feature = "copy-dylibs")]
			{
				let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
				copy_libraries(&lib_dir.join("lib"), &out_dir);
			}

			(lib_dir, true)
		}
		#[cfg(not(feature = "download-binaries"))]
		{
			println!("cargo:rustc-link-lib=add_ort_library_path_or_enable_feature_download-binaries_see_ort_docs");
			(PathBuf::default(), false)
		}
	}
}

fn real_main(link: bool) {
	println!("cargo:rerun-if-env-changed={}", ORT_ENV_SYSTEM_LIB_LOCATION);
	println!("cargo:rerun-if-env-changed={}", ORT_ENV_SYSTEM_LIB_PROFILE);

	let (install_dir, needs_link) = prepare_libort_dir();

	let lib_dir = if install_dir.join("lib").exists() { install_dir.join("lib") } else { install_dir };

	if link {
		if needs_link {
			println!("cargo:rustc-link-lib=onnxruntime");
			println!("cargo:rustc-link-search=native={}", lib_dir.display());
		}

		static_link_prerequisites(false);
	}
}

fn main() {
	if env::var("DOCS_RS").is_ok() {
		return;
	}

	if cfg!(feature = "load-dynamic") {
		// we only need to execute the real main step if we are using the download strategy...
		if cfg!(feature = "download-binaries") && env::var(ORT_ENV_SYSTEM_LIB_LOCATION).is_err() {
			// but we don't need to link to the binaries we download (so all we are doing is downloading them and placing them in
			// the output directory)
			real_main(false);
		}
	} else {
		// if we are not using the load-dynamic feature then we need to link to dylibs.
		real_main(true);
	}
}
