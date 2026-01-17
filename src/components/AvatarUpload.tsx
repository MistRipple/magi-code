import * as React from 'react';

/**
 * 头像上传组件属性
 */
interface AvatarUploadProps {
  /** 上传成功后的回调函数 */
  onUpload: (url: string) => void;
  /** 移除头像的回调函数 */
  onRemove?: () => void;
  /** 初始显示的头像 URL */
  initialAvatarUrl?: string;
  /** 最大文件限制（单位：MB），默认 2MB */
  maxSizeMB?: number;
  /** 容器类名 */
  className?: string;
}

/**
 * 现代用户头像上传组件
 * 
 * 特性：
 * 1. 支持点击上传和拖拽上传 (Drag & Drop)
 * 2. 具有悬停态交互，显示“编辑”提示
 * 3. 实时本地预览
 * 4. 优雅的加载动画 (CSS Spinner)
 * 5. 错误提示处理（文件过大等）
 * 6. 支持移除现有头像
 */
export const AvatarUpload: React.FC<AvatarUploadProps> = ({ 
  onUpload, 
  onRemove,
  initialAvatarUrl, 
  maxSizeMB = 2,
  className
}: AvatarUploadProps) => {
  const [preview, setPreview] = React.useState<string | undefined>(initialAvatarUrl);
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [isDragging, setIsDragging] = React.useState(false);
  const fileInputRef = React.useRef<HTMLInputElement>(null);

  // 初始化预览图
  React.useEffect(() => {
    setPreview(initialAvatarUrl);
  }, [initialAvatarUrl]);

  // 处理文件并开始上传
  const processFile = (file: File) => {
    setError(null);

    // 验证文件类型
    if (!file.type.startsWith('image/')) {
      setError('请选择有效的图片文件');
      return;
    }

    // 验证文件大小
    if (file.size > maxSizeMB * 1024 * 1024) {
      setError(`文件大小不能超过 ${maxSizeMB}MB`);
      return;
    }

    // 1. 设置本地预览
    const reader = new FileReader();
    reader.onloadend = () => {
      setPreview(reader.result as string);
    };
    reader.onerror = () => {
      setError('读取文件失败');
    };
    reader.readAsDataURL(file);
    
    // 2. 触发上传逻辑
    handleUpload(file);
  };

  // 处理文件选择事件
  const handleFileChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (file) {
      processFile(file);
    }
  };

  // 模拟文件上传过程
  const handleUpload = async (file: File) => {
    setLoading(true);
    try {
      // 模拟网络请求延迟
      await new Promise(resolve => setTimeout(resolve, 1500));
      
      // 模拟返回上传后的 URL
      const mockUrl = `https://storage.example.com/avatars/${Date.now()}_${file.name}`;
      onUpload(mockUrl);
    } catch (err) {
      console.error('上传失败:', err);
      setError('上传失败，请重试');
      // 如果失败，且之前没有初始图，则清空预览
      if (!initialAvatarUrl) setPreview(undefined);
    } finally {
      setLoading(false);
    }
  };

  // 处理移除头像
  const handleRemove = (e: React.MouseEvent) => {
    e.stopPropagation(); // 防止触发上传
    setPreview(undefined);
    setError(null);
    if (fileInputRef.current) {
      fileInputRef.current.value = '';
    }
    onRemove?.();
  };

  // 拖拽处理
  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault();
    setIsDragging(true);
  };

  const handleDragLeave = (e: React.DragEvent) => {
    e.preventDefault();
    setIsDragging(false);
  };

  const handleDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setIsDragging(false);
    const file = e.dataTransfer.files?.[0];
    if (file) {
      processFile(file);
    }
  };

  return (
    <div className={`avatar-upload-wrapper ${className || ''}`} style={styles.wrapper}>
      <div 
        style={{
          ...styles.container,
          ...(isDragging ? styles.containerDragging : {}),
          ...(error ? styles.containerError : {})
        }}
        onDragOver={handleDragOver}
        onDragLeave={handleDragLeave}
        onDrop={handleDrop}
        onClick={() => !loading && fileInputRef.current?.click()}
        role="button"
        aria-label="点击或拖拽上传头像"
        tabIndex={0}
        onKeyDown={(e: React.KeyboardEvent) => e.key === 'Enter' && !loading && fileInputRef.current?.click()}
      >
        {/* 头像预览 */}
        <div style={styles.previewArea}>
          {preview ? (
            <img src={preview} alt="头像预览" style={styles.image} />
          ) : (
            <div style={styles.placeholder}>
              <svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="#ccc" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                <path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2"></path>
                <circle cx="12" cy="7" r="4"></circle>
              </svg>
            </div>
          )}

          {/* 悬停态覆盖层 */}
          <div className="hover-overlay" style={styles.overlay}>
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"></path>
              <path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"></path>
            </svg>
            <span style={{ fontSize: '12px', marginTop: '4px' }}>更换头像</span>
          </div>
          
          {/* 加载中遮罩 */}
          {loading && (
            <div style={styles.loadingOverlay}>
              <div style={styles.spinner}></div>
            </div>
          )}
        </div>

        {/* 隐藏的文件输入框 */}
        <input 
          ref={fileInputRef}
          type="file" 
          accept="image/*" 
          onChange={handleFileChange} 
          disabled={loading}
          style={styles.hiddenInput} 
        />
      </div>

      {/* 底部操作和辅助信息 */}
      <div style={styles.infoArea}>
        {error ? (
          <span style={styles.errorText}>{error}</span>
        ) : (
          <span style={styles.helperText}>支持 JPG, PNG, GIF (最大 {maxSizeMB}MB)</span>
        )}
        
        {preview && !loading && (
          <button 
            type="button" 
            onClick={handleRemove} 
            style={styles.removeButton}
          >
            移除头像
          </button>
        )}
      </div>

      {/* 样式定义所需的一点内联 CSS 以处理 hover 效果 */}
      <style>{`
        .avatar-upload-wrapper .hover-overlay {
          opacity: 0;
          transition: opacity 0.2s ease-in-out;
        }
        .avatar-upload-wrapper div[role="button"]:hover .hover-overlay {
          opacity: 1;
        }
        @keyframes avatar-spin {
          to { transform: rotate(360deg); }
        }
      `}</style>
    </div>
  );
};

// 样式定义
const styles = {
  wrapper: {
    display: 'flex',
    flexDirection: 'column' as const,
    alignItems: 'center',
    gap: '12px',
    padding: '16px',
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif',
  },
  container: {
    position: 'relative' as const,
    width: '120px',
    height: '120px',
    borderRadius: '50%',
    cursor: 'pointer',
    border: '2px dashed #e0e0e0',
    padding: '4px',
    transition: 'all 0.2s ease',
    outline: 'none',
  },
  containerDragging: {
    borderColor: '#0070f3',
    backgroundColor: 'rgba(0, 112, 243, 0.05)',
    transform: 'scale(1.05)',
  },
  containerError: {
    borderColor: '#ff4d4f',
  },
  previewArea: {
    position: 'relative' as const,
    width: '100%',
    height: '100%',
    borderRadius: '50%',
    overflow: 'hidden',
    backgroundColor: '#fafafa',
    display: 'flex',
    justifyContent: 'center',
    alignItems: 'center',
  },
  image: {
    width: '100%',
    height: '100%',
    objectFit: 'cover' as const,
  },
  placeholder: {
    display: 'flex',
    flexDirection: 'column' as const,
    alignItems: 'center',
    justifyContent: 'center',
  },
  overlay: {
    position: 'absolute' as const,
    top: 0,
    left: 0,
    right: 0,
    bottom: 0,
    backgroundColor: 'rgba(0, 0, 0, 0.4)',
    color: 'white',
    display: 'flex',
    flexDirection: 'column' as const,
    justifyContent: 'center',
    alignItems: 'center',
    zIndex: 1,
  },
  loadingOverlay: {
    position: 'absolute' as const,
    top: 0,
    left: 0,
    right: 0,
    bottom: 0,
    backgroundColor: 'rgba(255, 255, 255, 0.8)',
    display: 'flex',
    justifyContent: 'center',
    alignItems: 'center',
    zIndex: 2,
  },
  spinner: {
    width: '24px',
    height: '24px',
    border: '3px solid #f3f3f3',
    borderTop: '3px solid #0070f3',
    borderRadius: '50%',
    animation: 'avatar-spin 1s linear infinite',
  },
  hiddenInput: {
    display: 'none',
  },
  infoArea: {
    display: 'flex',
    flexDirection: 'column' as const,
    alignItems: 'center',
    gap: '4px',
    minHeight: '40px',
  },
  helperText: {
    fontSize: '12px',
    color: '#8e8e93',
  },
  errorText: {
    fontSize: '12px',
    color: '#ff4d4f',
    fontWeight: 500,
  },
  removeButton: {
    background: 'none',
    border: 'none',
    color: '#0070f3',
    fontSize: '12px',
    cursor: 'pointer',
    padding: '4px 8px',
    marginTop: '4px',
    textDecoration: 'underline',
  },
};

export default AvatarUpload;