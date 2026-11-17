# 品牌与界面事实源

Magi 图标：仓库 web/public/brand/magi-icon.png，原文件内嵌至每份 HTML；不重画品牌。
当前产品截图：refs/magi-settings.png，经本地 daemon 正式入口打开设置/工具获得。
运行入口：http://127.0.0.1:38123/web.html；本轮只读浏览。
已采样主题：background rgba(252,252,251,1)，foreground #272725，foreground-muted #80807f，border rgba(39,39,37,.084)，sidebar #f5f5f4，main #f9f9f8，accent #2563eb，主按钮 #272725。原型辅助文字略加深以保持可读性。
字体：-apple-system, BlinkMacSystemFont, Segoe UI, Roboto, sans-serif；代码字体沿产品等宽字体。共享间距 2/4/8/12/16/24/32px；内容列 880px；分区标题 14px，正文 14px，辅助 12px；按钮 8px 圆角、面板 10px、边框 1px。实际导航宽度为用户已保存的 320px，原型保留此桌面尺度。
权威规则：docs/ux-ui-guidelines.md、web/AGENTS.md、web/src/styles/global.css、web/src/styles/settings.css。
三版使用同一组浅深色主题，差异在浏览结构，不凭空更换品牌风格。
