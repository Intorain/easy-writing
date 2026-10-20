//! 桌面应用的后台命令不创建 Windows 控制台，仍保留重定向的标准输入输出。
use std::process::Command;

pub(crate) fn hide_console(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：控制台子进程在后台执行，stdio 管道照常可用。
        // 不使用 DETACHED_PROCESS，也不把 Python 打包成禁用标准流的 windowed 程序。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
