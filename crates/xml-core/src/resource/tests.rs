use std::path::PathBuf;

use super::*;

fn directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("xml-core-resource-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn recognizes_network_paths() {
    for path in [
        r"\\server\share\a.xsd",
        "//server/share/a.xsd",
        r"\\?\UNC\server\share\a.xsd",
        r"\\.\PhysicalDrive0",
        r"/\server\share",
    ] {
        assert!(is_network_path(Path::new(path)), "{path}");
    }
    for path in [
        "/tmp/a.xsd",
        "a.xsd",
        "C:/a.xsd",
        r"C:\a.xsd",
        r"\\?\C:\a.xsd",
        "",
        "/",
    ] {
        assert!(!is_network_path(Path::new(path)), "{path}");
    }
}

#[test]
fn reads_bounded_regular_files_only() {
    let directory = directory("read");
    let file = directory.join("a.xsd");
    fs::write(&file, "<a>é</a>").unwrap();
    assert_eq!(read_text_file(&file, 100).as_deref(), Ok("<a>é</a>"));
    assert!(is_local_file(&file));
    assert!(!is_local_file(&directory));
    assert!(!is_local_file(Path::new("//server/share/a.xsd")));
    assert_eq!(
        read_text_file(&file, 4),
        Err(ResourceError::TooLarge { limit: 4 })
    );
    assert_eq!(
        read_text_file(&directory, 100),
        Err(ResourceError::NotAFile)
    );
    assert!(matches!(
        read_text_file(&directory.join("missing.xsd"), 100),
        Err(ResourceError::Io(_))
    ));
    let binary = directory.join("binary.xsd");
    fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
    assert_eq!(read_text_file(&binary, 100), Err(ResourceError::NotUtf8));
    assert_eq!(
        read_text_file(Path::new("//server/share/a.xsd"), 100),
        Err(ResourceError::NetworkPath)
    );
    let _ = fs::remove_dir_all(&directory);
}

#[cfg(unix)]
#[test]
fn refuses_links_to_devices_but_follows_links_to_files() {
    let directory = directory("links");
    let file = directory.join("a.xsd");
    fs::write(&file, "<a/>").unwrap();
    let to_file = directory.join("to-file.xsd");
    std::os::unix::fs::symlink(&file, &to_file).unwrap();
    assert_eq!(read_text_file(&to_file, 100).as_deref(), Ok("<a/>"));
    let to_device = directory.join("zero.xsd");
    std::os::unix::fs::symlink("/dev/zero", &to_device).unwrap();
    assert_eq!(
        read_text_file(&to_device, 100),
        Err(ResourceError::NotAFile)
    );
    assert_eq!(
        read_text_file(Path::new("/dev/zero"), 100),
        Err(ResourceError::NotAFile)
    );
    let _ = fs::remove_dir_all(&directory);
}
