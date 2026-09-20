#[cfg(windows)]
#[path = "../tests/support/pixel_fixture.rs"]
mod pixel_fixture;

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    // The example uses the same embedded PMv2 manifest as the application;
    // do not hide a missing manifest by overriding process/thread DPI here.
    color_picker::platform::windows::check_environment()?;
    if std::env::args_os()
        .skip(1)
        .any(|argument| argument == "--check-environment")
    {
        println!("pixel-fixture: PerMonitorV2 active");
        return Ok(());
    }
    let fixture = pixel_fixture::PixelFixture::new()?;
    println!("Physical origin: {:?}", fixture.origin()?);
    println!("R=x%256, G=y%256, B=(x^y)%256; final 16 rows: one-pixel RGB stripes.");
    println!("Use a controlled SDR desktop. Close the fixture window when finished.");
    fixture.run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The pixel fixture requires Windows.");
}
