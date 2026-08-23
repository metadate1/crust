//! Opt-in extractor for streams on a user-supplied retail disc image.

use std::{
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use crust_formats::disc::DiscImage;

fn main() -> ExitCode {
    let mut arguments = env::args_os();
    let program = arguments
        .next()
        .unwrap_or_else(|| OsString::from("extract-streams"));
    let arguments = arguments.collect::<Vec<_>>();

    if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("-h" | "--help")) {
        print_usage(&program);
        return ExitCode::SUCCESS;
    }
    if arguments.len() != 2 {
        eprintln!("error: expected a disc image and a new output directory");
        print_usage(&program);
        return ExitCode::FAILURE;
    }

    let disc_path = PathBuf::from(&arguments[0]);
    let output_path = PathBuf::from(&arguments[1]);
    match extract(&disc_path, &output_path) {
        Ok(summary) => {
            println!(
                "extracted {} canonical streams ({} bytes) into {}",
                summary.file_count,
                summary.byte_count,
                output_path.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage(program: &OsString) {
    eprintln!(
        "usage: {} <retail-disc.bin> <new-output-directory>",
        Path::new(program).display()
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExtractionSummary {
    file_count: usize,
    byte_count: u64,
}

fn extract(disc_path: &Path, output_path: &Path) -> io::Result<ExtractionSummary> {
    if output_path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output path must name a directory",
        ));
    }
    require_absent(output_path)?;

    let disc_bytes = fs::read(disc_path).map_err(|error| {
        contextual_error(
            &error,
            &format!("could not read disc image {}", disc_path.display()),
        )
    })?;
    let disc = DiscImage::open(&disc_bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("could not open disc image {}: {error}", disc_path.display()),
        )
    })?;
    let streams = disc.discover_streams().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("could not discover retail streams: {error}"),
        )
    })?;
    streams.validate_complete_retail().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("disc does not contain the exact retail stream catalog: {error}"),
        )
    })?;

    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        contextual_error(
            &error,
            &format!("could not create output parent {}", parent.display()),
        )
    })?;
    let staging = StagingDirectory::create_next_to(output_path)?;
    let staged = (|| {
        let mut byte_count = 0_u64;
        for stream in streams.files() {
            let bytes = disc.read_stream(stream).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("could not read {} from the disc: {error}", stream.name),
                )
            })?;
            let filename = stream.name.filename();
            let path = staging.path().join(&filename);
            write_new_file(&path, &bytes)?;
            byte_count = byte_count
                .checked_add(u64::try_from(bytes.len()).expect("usize fits u64"))
                .ok_or_else(|| io::Error::other("extracted byte count overflows u64"))?;
        }
        Ok((streams.files().len(), byte_count))
    })();
    let (file_count, byte_count) = match staged {
        Ok(summary) => summary,
        Err(error) => return staging.finish(Err(error)),
    };
    staging.persist(output_path)?;
    Ok(ExtractionSummary {
        file_count,
        byte_count,
    })
}

fn require_absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "refusing to overwrite existing output path {}; choose a new directory",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(contextual_error(
            &error,
            &format!("could not inspect output path {}", path.display()),
        )),
    }
}

fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        contextual_error(&error, &format!("could not create {}", path.display()))
    })?;
    file.write_all(bytes)
        .map_err(|error| contextual_error(&error, &format!("could not write {}", path.display())))
}

fn copy_new_file(source_path: &Path, output_path: &Path) -> io::Result<()> {
    let mut source = File::open(source_path).map_err(|error| {
        contextual_error(
            &error,
            &format!("could not open staged file {}", source_path.display()),
        )
    })?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(output_path).map_err(|error| {
        contextual_error(
            &error,
            &format!("could not create published file {}", output_path.display()),
        )
    })?;
    io::copy(&mut source, &mut output)
        .map(|_| ())
        .map_err(|error| {
            contextual_error(
                &error,
                &format!(
                    "could not publish {}; the newly-created output file was preserved for manual inspection",
                    output_path.display()
                ),
            )
        })
}

fn publish_new_file(source_path: &Path, output_path: &Path) -> io::Result<()> {
    match fs::hard_link(source_path, output_path) {
        Ok(()) => Ok(()),
        // Staging and output are siblings, but hard links are not supported by
        // every filesystem. The fallback retains create-new/no-clobber
        // semantics without making filesystem support part of the contract.
        Err(_) => copy_new_file(source_path, output_path),
    }
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

#[derive(Debug)]
struct StagingDirectory {
    path: Option<PathBuf>,
    cleanup_failure_reported: bool,
}

impl StagingDirectory {
    fn create_next_to(output_path: &Path) -> io::Result<Self> {
        let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
        let output_name = output_path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "output path must name a directory",
            )
        })?;
        let stem = output_name.to_string_lossy();
        for attempt in 0..1_000_u16 {
            let candidate = parent.join(format!(
                ".{stem}.crust-extract-{}-{attempt}",
                std::process::id()
            ));
            match create_private_directory(&candidate) {
                Ok(()) => {
                    return Ok(Self {
                        path: Some(candidate),
                        cleanup_failure_reported: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(contextual_error(
                        &error,
                        &format!("could not create staging directory in {}", parent.display()),
                    ));
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "could not reserve a staging directory in {}",
                parent.display()
            ),
        ))
    }

    fn path(&self) -> &Path {
        self.path.as_deref().expect("staging directory is active")
    }

    fn persist(self, output_path: &Path) -> io::Result<()> {
        let publication = OutputDirectory::publish_from(self.path(), output_path);
        self.finish(publication)
    }

    fn finish<T>(mut self, operation: io::Result<T>) -> io::Result<T> {
        let cleanup = self.remove();
        if cleanup.is_err() {
            self.cleanup_failure_reported = true;
        }
        merge_operation_and_cleanup(operation, cleanup)
    }

    fn remove(&mut self) -> io::Result<()> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        match fs::remove_dir_all(path) {
            Ok(()) => {
                self.path = None;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.path = None;
                Ok(())
            }
            Err(error) => Err(contextual_error(
                &error,
                &format!(
                    "could not remove private staging directory {}; it may remain and must be inspected manually",
                    path.display()
                ),
            )),
        }
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if self.cleanup_failure_reported {
            return;
        }
        if let Err(error) = self.remove() {
            let _ = writeln!(io::stderr().lock(), "warning: {error}");
        }
    }
}

#[derive(Debug)]
struct OutputDirectory {
    path: PathBuf,
    published_file_count: usize,
    committed: bool,
    failure_reported: bool,
}

impl OutputDirectory {
    fn publish_from(source: &Path, output_path: &Path) -> io::Result<()> {
        let mut output = Self::claim(output_path)?;
        if let Err(error) = output.copy_from(source) {
            return Err(output.preserve_failure(&error));
        }
        output.commit();
        Ok(())
    }

    fn claim(path: &Path) -> io::Result<Self> {
        create_private_directory(path).map_err(|error| {
            contextual_error(
                &error,
                &format!(
                    "could not claim new output directory {}; refusing to overwrite it",
                    path.display()
                ),
            )
        })?;
        Ok(Self {
            path: path.to_owned(),
            published_file_count: 0,
            committed: false,
            failure_reported: false,
        })
    }

    fn copy_from(&mut self, source: &Path) -> io::Result<()> {
        let mut entries = fs::read_dir(source)
            .map_err(|error| {
                contextual_error(
                    &error,
                    &format!("could not read staging directory {}", source.display()),
                )
            })?
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);

        for entry in entries {
            let source_path = entry.path();
            if !entry.file_type()?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "staging entry {} is not a regular file",
                        source_path.display()
                    ),
                ));
            }
            let output_path = self.path.join(entry.file_name());
            publish_new_file(&source_path, &output_path)?;
            self.published_file_count += 1;
        }
        Ok(())
    }

    fn preserve_failure(mut self, error: &io::Error) -> io::Error {
        self.failure_reported = true;
        contextual_error(
            error,
            &format!(
                "publication stopped after {} completed file(s); claimed output directory {} was deliberately preserved for manual inspection",
                self.published_file_count,
                self.path.display()
            ),
        )
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for OutputDirectory {
    fn drop(&mut self) {
        if !self.committed && !self.failure_reported {
            let _ = writeln!(
                io::stderr().lock(),
                "warning: extraction stopped unexpectedly after claiming {}; partial output was deliberately preserved for manual inspection",
                self.path.display()
            );
        }
    }
}

fn merge_operation_and_cleanup<T>(
    operation: io::Result<T>,
    cleanup: io::Result<()>,
) -> io::Result<T> {
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(error), Err(cleanup_error)) => Err(io::Error::new(
            error.kind(),
            format!("{error}; additionally, staging cleanup failed: {cleanup_error}"),
        )),
    }
}

fn contextual_error(error: &io::Error, context: &str) -> io::Error {
    io::Error::new(error.kind(), format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU32 = AtomicU32::new(0);

    #[derive(Debug)]
    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            let base = std::env::temp_dir();
            for _ in 0..1_000 {
                let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = base.join(format!(
                    "crust-extract-streams-test-{}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("could not create test directory: {error}"),
                }
            }
            panic!("could not reserve a unique extractor test directory");
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn publishing_preserves_canonical_files_and_removes_staging() {
        let root = TestDirectory::create();
        let output = root.path().join("streams");
        let staging = StagingDirectory::create_next_to(&output).expect("staging must be created");
        let staging_path = staging.path().to_owned();
        write_new_file(&staging_path.join("s0000009.nsd"), b"NSD")
            .expect("synthetic NSD must be staged");
        write_new_file(&staging_path.join("s0000009.nsf"), b"NSF")
            .expect("synthetic NSF must be staged");

        staging.persist(&output).expect("publish must succeed");

        assert!(!staging_path.exists());
        assert_eq!(
            fs::read(output.join("s0000009.nsd")).expect("published NSD must be readable"),
            b"NSD"
        );
        assert_eq!(
            fs::read(output.join("s0000009.nsf")).expect("published NSF must be readable"),
            b"NSF"
        );
    }

    #[test]
    fn publishing_refuses_a_destination_created_after_staging() {
        let root = TestDirectory::create();
        let output = root.path().join("streams");
        let staging = StagingDirectory::create_next_to(&output).expect("staging must be created");
        let staging_path = staging.path().to_owned();
        write_new_file(&staging_path.join("s0000009.nsd"), b"staged")
            .expect("synthetic stream must be staged");
        fs::create_dir(&output).expect("racing destination must be created");

        let error = staging
            .persist(&output)
            .expect_err("publish must refuse the raced destination");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            fs::read_dir(&output)
                .expect("racing destination must remain a directory")
                .next()
                .is_none(),
            "racing destination must remain empty"
        );
        assert!(!output.join("s0000009.nsd").exists());
        assert!(!staging_path.exists());
    }

    #[test]
    fn publishing_a_file_refuses_to_replace_existing_data() {
        let root = TestDirectory::create();
        let source = root.path().join("staged.nsd");
        let output = root.path().join("published.nsd");
        write_new_file(&source, b"staged").expect("staged file must be created");
        write_new_file(&output, b"owned-by-caller").expect("caller file must be created");

        let error = publish_new_file(&source, &output)
            .expect_err("publication must refuse the existing file");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(output).expect("caller file must remain readable"),
            b"owned-by-caller"
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_fallback_preserves_its_new_output_when_copying_fails() {
        let root = TestDirectory::create();
        let unreadable_as_a_file = root.path().join("source-directory");
        let output = root.path().join("published.nsd");
        fs::create_dir(&unreadable_as_a_file).expect("source directory must be created");

        let error = copy_new_file(&unreadable_as_a_file, &output)
            .expect_err("copying bytes from a directory must fail");

        assert_eq!(error.kind(), io::ErrorKind::IsADirectory);
        assert!(
            error
                .to_string()
                .contains("preserved for manual inspection")
        );
        assert_eq!(
            fs::metadata(&output)
                .expect("partial output must remain present")
                .len(),
            0
        );
    }

    #[test]
    fn failed_publication_preserves_a_replacement_for_a_published_file() {
        let root = TestDirectory::create();
        let source = root.path().join("staging");
        let output = root.path().join("streams");
        fs::create_dir(&source).expect("synthetic staging directory must be created");
        let first_name = "s0000008.nsd";
        let blocked_name = "s0000009.nsd";
        write_new_file(&source.join(first_name), b"published")
            .expect("first synthetic stream must be staged");
        write_new_file(&source.join(blocked_name), b"staged")
            .expect("blocked synthetic stream must be staged");

        let mut publication = OutputDirectory::claim(&output).expect("output must be claimed");
        write_new_file(&output.join(blocked_name), b"unrelated")
            .expect("an unrelated collision must be installed");
        let publication_error = publication
            .copy_from(&source)
            .expect_err("the second stream must collide after the first is published");
        let published_path = output.join(first_name);
        fs::remove_file(&published_path).expect("published name must be released");
        write_new_file(&published_path, b"replacement").expect("replacement must be installed");
        let error = publication.preserve_failure(&publication_error);

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(error.to_string().contains("after 1 completed file(s)"));
        assert_eq!(
            fs::read(&published_path).expect("replacement must remain readable"),
            b"replacement"
        );
        assert_eq!(
            fs::read(output.join(blocked_name)).expect("unrelated file must remain readable"),
            b"unrelated"
        );
    }

    #[test]
    fn staging_cleanup_failure_is_returned_without_removing_a_replacement() {
        let root = TestDirectory::create();
        let output = root.path().join("streams");
        let staging = StagingDirectory::create_next_to(&output).expect("staging must be created");
        let staging_path = staging.path().to_owned();
        fs::remove_dir(&staging_path).expect("empty staging directory must be removed");
        write_new_file(&staging_path, b"replacement").expect("replacement must be installed");

        let error = staging
            .finish::<()>(Err(io::Error::other("primary operation failed")))
            .expect_err("cleanup failure must be combined with the operation failure");

        assert!(error.to_string().contains("primary operation failed"));
        assert!(error.to_string().contains("staging cleanup failed"));
        assert!(error.to_string().contains("staging directory"));
        assert_eq!(
            fs::read(&staging_path).expect("replacement must remain readable"),
            b"replacement"
        );
    }
}
