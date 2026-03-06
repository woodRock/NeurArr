use std::env;
fn main() {
    let current = env::current_exe().unwrap();
    println!("Before rename: {}", current.display());
    
    #[cfg(target_os = "windows")]
    {
        let old = current.with_extension("old.exe");
        std::fs::rename(&current, &old).unwrap();
    }
    
    let after = env::current_exe().unwrap();
    println!("After rename: {}", after.display());
}
