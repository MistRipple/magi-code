//! 开发辅助：用与安装时完全相同的校验检查一个 ZIP 包，并打印清单摘要。
//! 用法：cargo run -p magi-plugin-system --example validate_package -- <包.zip>
use magi_plugin_system::PluginPackage;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("用法：validate_package <包.zip>");
    let bytes = std::fs::read(&path).expect("读取失败");
    match PluginPackage::from_archive(&bytes) {
        Ok(package) => {
            let manifest = package.manifest();
            println!(
                "通过校验：{} {} 摘要 {}",
                manifest.id,
                manifest.version,
                package.digest()
            );
            println!(
                "工具 {} 命令 {} 模型数据源 {} 视图 {}",
                manifest.contributions.tools.len(),
                manifest.contributions.commands.len(),
                manifest.contributions.model_sources.len(),
                manifest.contributions.views.len()
            );
        }
        Err(error) => {
            eprintln!("校验失败：{error}");
            std::process::exit(1);
        }
    }
}
