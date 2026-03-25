/**
 * 原子写入工具
 *
 * 通过 write-to-temp + rename 模式保证崩溃安全：
 * - 先将数据写入同目录下的临时文件
 * - 再通过 fs.renameSync 原子替换目标文件
 * - 如果写入过程中进程崩溃，目标文件不会被截断或损坏
 *
 * rename 在 POSIX 文件系统上是原子操作，在 Windows NTFS 上同样安全。
 */

import * as fs from 'fs';
import * as path from 'path';

/**
 * 原子写入文件（同步）
 *
 * @param filePath 目标文件路径
 * @param data 要写入的内容
 * @param encoding 编码，默认 utf-8
 */
export function atomicWriteFileSync(
  filePath: string,
  data: string | Buffer,
  encoding: BufferEncoding = 'utf-8',
): void {
  const dir = path.dirname(filePath);
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }

  // 临时文件名：.{basename}.{pid}-{timestamp}-{random}.tmp
  const basename = path.basename(filePath);
  const tmpName = `.${basename}.${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}.tmp`;
  const tmpPath = path.join(dir, tmpName);

  try {
    if (typeof data === 'string') {
      fs.writeFileSync(tmpPath, data, encoding);
    } else {
      fs.writeFileSync(tmpPath, data);
    }
    fs.renameSync(tmpPath, filePath);
  } catch (error) {
    // 清理临时文件（如果还存在）
    try {
      if (fs.existsSync(tmpPath)) {
        fs.unlinkSync(tmpPath);
      }
    } catch {
      // 清理失败不阻塞主错误抛出
    }
    throw error;
  }
}

export async function atomicWriteFile(
  filePath: string,
  data: string | Buffer,
  encoding: BufferEncoding = 'utf-8',
): Promise<void> {
  const dir = path.dirname(filePath);
  await fs.promises.mkdir(dir, { recursive: true });

  const basename = path.basename(filePath);
  const tmpName = `.${basename}.${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}.tmp`;
  const tmpPath = path.join(dir, tmpName);

  try {
    if (typeof data === 'string') {
      await fs.promises.writeFile(tmpPath, data, encoding);
    } else {
      await fs.promises.writeFile(tmpPath, data);
    }
    await fs.promises.rename(tmpPath, filePath);
  } catch (error) {
    try {
      await fs.promises.rm(tmpPath, { force: true });
    } catch {
      // 清理失败不阻塞主错误抛出
    }
    throw error;
  }
}
