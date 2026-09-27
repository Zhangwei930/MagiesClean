// Windows Release 下不显示控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    magies_clean_lib::run()
}
