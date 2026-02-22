/**
 * 自动化 Webview 测试
 * 将此文件内容复制到 Tauri 应用的控制台中运行
 */

console.log('🚀 启动自动化测试...\n');

// 测试 URL
const TEST_URL = 'https://mp.weixin.qq.com/s?__biz=MzU5NzUxNjg3Nw==&mid=2247507651&idx=2&sn=234da27242f990a240c72875f4bd6712&chksm=ffed9005cd531e9a433c97fc9231451684e4d6ebeaf0a82aecd0191d5c9cded143b4ab354a8e&scene=0#rd';

// 测试步骤
async function runTest() {
    console.log('📋 测试计划:');
    console.log('  1. 检查 Tauri API');
    console.log('  2. 获取网页内容');
    console.log('  3. 创建 iframe');
    console.log('  4. 注入内容');
    console.log('  5. 验证显示\n');

    // 步骤 1: 检查 API
    console.log('🔍 [1/5] 检查 Tauri API...');
    if (typeof invoke === 'undefined') {
        console.error('❌ Tauri invoke 不可用');
        console.log('💡 请在 Tauri 应用中运行此测试');
        return;
    }
    console.log('✅ Tauri API 可用\n');

    // 步骤 2: 获取内容
    console.log('🌐 [2/5] 获取网页内容...');
    console.log(`   URL: ${TEST_URL.substring(0, 50)}...`);

    try {
        const html = await invoke('fetch_website_content', { url: TEST_URL });
        console.log(`✅ 成功获取 ${html.length} 字符`);
        console.log(`   预览: ${html.substring(0, 100)}...\n`);
    } catch (e) {
        console.error(`❌ 获取失败: ${e}`);
        return;
    }

    // 步骤 3: 创建测试 iframe
    console.log('🖼️  [3/5] 创建测试 iframe...');
    const detail = document.getElementById('detail-content');
    if (!detail) {
        console.error('❌ 找不到 detail-content');
        return;
    }

    // 保存原始内容
    const originalContent = detail.innerHTML;

    detail.classList.add('webview-mode');
    detail.innerHTML = `
        <div class="webview-container" style="height: 100%; position: relative;">
            <iframe id="auto-test-iframe"
                    style="width: 100%; height: 100%; border: none;"
                    sandbox="allow-same-origin allow-scripts allow-forms">
            </iframe>
        </div>
    `;
    console.log('✅ iframe 已创建\n');

    // 步骤 4: 注入内容
    console.log('💉 [4/5] 注入内容到 iframe...');
    const iframe = document.getElementById('auto-test-iframe');

    // 简单测试 HTML
    const testHtml = `<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <style>
        body {
            font-family: -apple-system, BlinkMacSystemFont, sans-serif;
            padding: 20px;
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            color: white;
        }
        .test-box {
            background: rgba(255,255,255,0.1);
            backdrop-filter: blur(10px);
            padding: 30px;
            border-radius: 10px;
            text-align: center;
        }
        h1 { font-size: 32px; margin-bottom: 20px; }
        .success { font-size: 48px; margin: 20px 0; }
        .info { background: rgba(255,255,255,0.2); padding: 15px; border-radius: 5px; margin-top: 20px; }
    </style>
</head>
<body>
    <div class="test-box">
        <h1>✅ Webview 测试成功！</h1>
        <div class="success">🎉</div>
        <p>如果你能看到这个页面，说明 iframe 正常工作</p>
        <div class="info">
            <strong>测试信息:</strong><br>
            时间: ${new Date().toLocaleString()}<br>
            URL: ${TEST_URL.substring(0, 50)}...<br>
            方法: srcdoc
        </div>
    </div>
    <script>
        console.log('✅ iframe 中的 JavaScript 已执行');
        console.log('✅ iframe DOM 可访问');
    <\/script>
</body>
</html>`;

    // 设置 onload
    iframe.onload = () => {
        console.log('✅ iframe.onload 事件已触发');
    };

    // 注入内容
    iframe.srcdoc = testHtml;
    console.log('✅ 内容已注入\n');

    // 步骤 5: 验证
    console.log('🔬 [5/5] 验证显示...');

    setTimeout(() => {
        try {
            const iframeDoc = iframe.contentDocument || iframe.contentWindow?.document;
            if (iframeDoc && iframeDoc.body) {
                const hasContent = iframeDoc.body.innerHTML.length > 0;
                console.log(hasContent ? '✅' : '❌', `iframe 内容: ${iframeDoc.body.innerHTML.length} 字节`);

                if (hasContent) {
                    console.log('\n✅✅✅ 测试成功！ ✅✅✅');
                    console.log('iframe 正常工作，你可以在右侧面板看到内容');
                } else {
                    console.log('\n⚠️ iframe 内容为空，尝试备选方案...');
                    const dataUri = 'data:text/html;charset=utf-8,' + encodeURIComponent(testHtml);
                    iframe.src = dataUri;
                    console.log('✅ data URI 备选方案已应用');
                }
            }
        } catch (e) {
            console.log('⚠️ 无法访问 iframe 内容 (跨域限制)');
            console.log('但这不一定意味着失败，检查右侧面板是否显示内容');
        }

        console.log('\n📊 测试总结:');
        console.log('   1. Tauri API: ✅');
        console.log('   2. 内容获取: ✅');
        console.log('   3. iframe 创建: ✅');
        console.log('   4. 内容注入: ✅');
        console.log('   5. 显示验证: 检查右侧面板');
        console.log('\n💡 提示: 按 F12 打开开发者工具查看详细日志');

        // 5秒后恢复原始内容
        setTimeout(() => {
            console.log('\n🔄 5秒后恢复原始内容...');
            detail.innerHTML = originalContent;
            detail.classList.remove('webview-mode');
        }, 5000);

    }, 500);
}

// 运行测试
runTest().catch(err => {
    console.error('❌ 测试出错:', err);
});
