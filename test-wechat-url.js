/**
 * 微信公众号链接测试脚本
 * 在 Tauri 应用的开发者工具控制台中运行此脚本
 */

(async function testWeChatUrl() {
    const testUrl = 'https://mp.weixin.qq.com/s?__biz=MzU5NzUxNjg3Nw==&mid=2247507651&idx=2&sn=234da27242f990a240c72875f4bd6712&chksm=ffed9005cd531e9a433c97fc9231451684e4d6ebeaf0a82aecd0191d5c9cded143b4ab354a8e&scene=0#rd';

    console.log('='.repeat(60));
    console.log('🧪 开始微信公众号链接测试');
    console.log('测试 URL:', testUrl);
    console.log('='.repeat(60));

    // 检查 Tauri API 是否可用
    if (typeof invoke === 'undefined') {
        console.error('❌ Tauri API 不可用');
        console.log('请在 Tauri 应用中运行此测试，而不是在普通浏览器中');
        return;
    }

    try {
        console.log('\n[步骤 1] 调用后端获取网页内容...');
        const startTime = Date.now();

        const htmlContent = await invoke('fetch_website_content', { url: testUrl });

        const endTime = Date.now();
        console.log(`✅ 内容获取成功！`);
        console.log(`   - 内容长度: ${htmlContent.length} 字符`);
        console.log(`   - 耗时: ${endTime - startTime}ms`);
        console.log(`   - 前 500 字符预览:`);
        console.log('   ' + htmlContent.substring(0, 500).split('\n').join('\n   '));

        // 显示在 detail-content 中
        console.log('\n[步骤 2] 在页面中显示内容...');
        const detail = document.getElementById('detail-content');
        if (!detail) {
            console.error('❌ 找不到 detail-content 元素');
            return;
        }

        detail.classList.add('webview-mode');
        detail.innerHTML = `
            <div class="webview-container">
                <div class="webview-loading">测试: 显示微信公众号内容...</div>
                <iframe id="test-iframe" style="width: 100%; height: 100%; border: none;" sandbox="allow-same-origin allow-scripts allow-forms allow-popups"></iframe>
            </div>
        `;

        const iframe = document.getElementById('test-iframe');
        const loadingEl = detail.querySelector('.webview-loading');

        // 注入修复后的 HTML
        console.log('\n[步骤 3] 注入内容到 iframe...');

        // 创建完整的 HTML 文档
        const fullHtml = `<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <style>
    * { box-sizing: border-box; }
    html, body {
      background-color: #ffffff !important;
      color: #333333 !important;
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', sans-serif !important;
      font-size: 16px !important;
      line-height: 1.8 !important;
      padding: 16px !important;
      margin: 0 !important;
      word-wrap: break-word !important;
      overflow-wrap: break-word !important;
    }
    img { max-width: 100% !important; height: auto !important; display: block !important; margin: 10px 0 !important; }
    p { margin: 12px 0 !important; line-height: 1.8 !important; }
    a { color: #1a73e8 !important; text-decoration: underline !important; }
    .rich_media_content { color: #333333 !important; }
  </style>
</head>
<body>
${htmlContent}
</body>
</html>`;

        // 绑定 onload 事件
        iframe.onload = () => {
            console.log('✅ iframe onload 事件触发');
            if (loadingEl) {
                loadingEl.style.display = 'none';
            }
        };

        // 设置超时保护
        setTimeout(() => {
            if (loadingEl && loadingEl.style.display !== 'none') {
                console.log('⏰ 超时 - 隐藏加载提示');
                loadingEl.style.display = 'none';
            }
        }, 3000);

        // 先尝试 srcdoc
        iframe.srcdoc = fullHtml;
        console.log('✅ srcdoc 已设置');

        // 检查是否需要使用 data URI 备选方案
        setTimeout(() => {
            try {
                const iframeDoc = iframe.contentDocument || iframe.contentWindow?.document;
                if (iframeDoc && iframeDoc.body) {
                    const bodyLength = iframeDoc.body.innerHTML.length;
                    console.log(`✅ iframe 内容可访问，body 长度: ${bodyLength}`);

                    if (bodyLength === 0 && fullHtml.length > 0) {
                        console.warn('⚠️ srcdoc 似乎不工作，尝试 data URI 备选方案...');
                        const dataUri = 'data:text/html;charset=utf-8,' + encodeURIComponent(fullHtml);
                        iframe.src = dataUri;
                        console.log('✅ data URI 已设置');
                    }
                } else {
                    console.warn('⚠️ 无法访问 iframe 内容（跨域限制）');
                }
            } catch (e) {
                console.warn('⚠️ iframe 访问错误:', e.message);
                console.log('ℹ️  尝试 data URI 备选方案...');
                const dataUri = 'data:text/html;charset=utf-8,' + encodeURIComponent(fullHtml);
                iframe.src = dataUri;
                console.log('✅ data URI 已设置');
            }
        }, 500);

        console.log('\n[步骤 4] 测试完成！');
        console.log('✅ 检查右侧详情面板，应该能看到内容');
        console.log('✅ 如果仍然白屏，请检查:');
        console.log('   1. 浏览器控制台是否有错误');
        console.log('   2. iframe 元素是否在 DOM 中');
        console.log('   3. 网络请求是否成功');

    } catch (error) {
        console.error('\n❌ 测试失败:', error);
        console.error('错误详情:', error.message || error);
        console.log('\n可能的原因:');
        console.log('1. 网络连接问题');
        console.log('2. 微信公众号访问限制');
        console.log('3. 后端服务未启动');
    }

    console.log('\n' + '='.repeat(60));
    console.log('测试日志已输出到控制台');
    console.log('='.repeat(60));
})();
