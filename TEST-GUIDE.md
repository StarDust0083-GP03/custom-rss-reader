# 🧪 Webview 测试指南

## 测试 URL
微信公众号文章链接：
```
https://mp.weixin.qq.com/s?__biz=MzU5NzUxNjg3Nw==&mid=2247507651&idx=2&sn=234da27242f990a240c72875f4bd6712&chksm=ffed9005cd531e9a433c97fc9231451684e4d6ebeaf0a82aecd0191d5c9cded143b4ab354a8e&scene=0#rd
```

## 方法 1: 独立测试工具（推荐）

在浏览器中打开测试页面：
```bash
xdg-open test-iframe.html
```

这个工具包含：
- ✅ 基本 srcdoc 功能测试
- ✅ Data URI 备选方案测试
- ✅ 微信文章模拟测试
- ✅ 复杂 HTML 内容测试
- ✅ 实时测试日志

## 方法 2: Tauri 应用中测试

### 步骤 1: 启动应用
开发服务器已在运行: `http://localhost:1420/`

启动 Tauri 应用：
```bash
cd src-tauri
cargo run
```

### 步骤 2: 打开开发者工具
在应用中按 `F12` 打开开发者工具

### 步骤 3: 运行自动化测试

在控制台中复制粘贴以下代码：

```javascript
// 快速测试
(async function() {
    const url = 'https://mp.weixin.qq.com/s?__biz=MzU5NzUxNjg3Nw==&mid=2247507651&idx=2&sn=234da27242f990a240c72875f4bd6712&chksm=ffed9005cd531e9a433c97fc9231451684e4d6ebeaf0a82aecd0191d5c9cded143b4ab354a8e&scene=0#rd';

    // 获取内容
    const html = await invoke('fetch_website_content', { url });
    console.log('✅ 获取内容:', html.length, '字符');

    // 创建测试 iframe
    const detail = document.getElementById('detail-content');
    detail.innerHTML = `<iframe id="test" style="width:100%;height:500px;"></iframe>`;
    const iframe = document.getElementById('test');

    // 注入内容
    iframe.onload = () => console.log('✅ iframe 已加载');
    iframe.srcdoc = `<!DOCTYPE html><html><head><meta charset="utf-8"><style>body{padding:20px;font-family:sans-serif;background:linear-gradient(135deg,#667eea,#764ba2);color:white;}</style></head><body><h1>✅ 测试成功!</h1><p>时间: ${new Date().toLocaleString()}</p></body></html>`;

    // 检查
    setTimeout(() => {
        try {
            const doc = iframe.contentDocument || iframe.contentWindow.document;
            console.log('✅ iframe 可访问:', doc.body.innerHTML.length, '字节');
        } catch(e) {
            console.log('⚠️ 跨域限制(正常)');
        }
    }, 500);
})();
```

## 方法 3: 使用内置测试函数

在控制台中运行：

```javascript
// 测试基本功能
testIframeBasic()

// 测试完整 webview
testWebview()
```

## 预期结果

### ✅ 成功标志：
1. 控制台显示 "srcdoc set successfully"
2. 控制台显示 "iframe onload fired"
3. 右侧详情面板显示内容（不是白屏）
4. 看到测试页面的标题和内容

### ⚠️ 备选方案触发：
如果 srcdoc 不工作，会看到：
```
[WebContent] srcdoc appears not working, trying data URI fallback
```
然后会自动使用 data URI 方案。

### ❌ 失败标志：
- 长时间显示 "Loading webpage..."
- 完全白屏
- 控制台有红色错误信息

## 修复内容总结

| 问题 | 修复方案 |
|------|----------|
| Webview 白屏 | 1. 移除初始 display:none<br>2. 先绑定 onload 再设置 srcdoc<br>3. 添加 data URI 备选方案 |
| 翻译框截断 | max-height: 300px → 50vh |
| 缺少调试信息 | 添加详细日志和测试函数 |

## 调试命令

```javascript
// 检查 iframe 元素
document.getElementById('content-iframe')

// 检查 webview 容器
document.querySelector('.webview-container')

// 检查加载状态
document.querySelector('.webview-loading')

// 手动触发测试
testWebview('你的URL')
```

## 常见问题

**Q: 仍然白屏怎么办？**
A:
1. 检查控制台是否有错误
2. 检查网络请求是否成功
3. 尝试刷新页面 (Ctrl+R)
4. 尝试使用 data URI 方案

**Q: 如何查看详细日志？**
A: 按 F12 打开开发者工具，查看 Console 标签

**Q: 测试成功但实际使用失败？**
A: 可能是特定网站的内容问题，检查控制台的具体错误信息
