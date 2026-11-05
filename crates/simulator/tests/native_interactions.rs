//! Exercises the real Slint pointer/gesture and command-queue path offscreen.

#[test]
fn native_touch_and_settings_acknowledgements() {
    njorun_simulator::ui::qualify_ui_interactions().expect("native interaction qualification");
}
