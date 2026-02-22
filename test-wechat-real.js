/**
 * 直接测试微信公众号真实内容
 * 在 Tauri 应用控制台中运行
 */

(async function testRealWeChatContent() {
    console.clear();
    console.log('🔍 开始测试微信公众号真实内容...\n');

    const testUrl = 'https://mp.weixin.qq.com/s?__biz=MzU5NzUxNjg3Nw==&mid=2247507651&idx=2&sn=234da27242f990a240c72875f4bd6712&chksm=ffed9005cd531e9a433c97fc9231451684e4d6ebeaf0a82aecd0191d5c9cded143b4ab354a8e&scene=0#rd';

    // 步骤 1: 获取真实内容
    console.log('[步骤 1] 获取微信公众号内容...');
    let realContent = '';
    try {
        realContent = await invoke('fetch_website_content', { url: testUrl });
        console.log('✅ 获取成功，长度:', realContent.length, '字符');
        console.log('前 300 字符预览:');
        console.log(realContent.substring(0, 300));
        console.log('\n');
    } catch (e) {
        console.error('❌ 获取失败:', e);
        return;
    }

    // 步骤 2: 创建测试界面
    console.log('[步骤 2] 创建测试界面...\n');
    const detail = document.getElementById('detail-content');
    const originalContent = detail.innerHTML;

    detail.classList.add('webview-mode');
    detail.innerHTML = `
        <div style="display: flex; flex-direction: column; height: 100%; gap: 10px; padding: 10px; background: #f5f5f5; overflow-y: auto;">
            <div id="status-box" style="padding: 15px; background: white; border-radius: 8px; font-family: monospace; font-size: 13px; box-shadow: 0 2px 4px rgba(0,0,0,0.1);">
                <strong>🧪 微信公众号真实内容测试</strong><br>
                正在初始化...
            </div>

            <div style="display: grid; grid-template-columns: 1fr 1fr; gap: 10px; flex: 1;">
                <!-- 测试 1: 原始内容 -->
                <div style="display: flex; flex-direction: column; background: white; border-radius: 8px; overflow: hidden;">
                    <div style="padding: 10px; background: #667eea; color: white; font-size: 12px; font-weight: bold;">
                        方法 1: 原始 HTML (无处理)
                    </div>
                    <iframe id="test1" style="flex: 1; border: none;"></iframe>
                </div>

                <!-- 测试 2: fixHtmlContent 处理 -->
                <div style="display: flex; flex-direction: column; background: white; border-radius: 8px; overflow: hidden;">
                    <div style="padding: 10px; background: #764ba2; color: white; font-size: 12px; font-weight: bold;">
                        方法 2: fixHtmlContent 处理后
                    </div>
                    <iframe id="test2" style="flex: 1; border: none;"></iframe>
                </div>

                <!-- 测试 3: 简化版 -->
                <div style="display: flex; flex-direction: column; background: white; border-radius: 8px; overflow: hidden;">
                    <div style="padding: 10px; background: #f093fb; color: white; font-size: 12px; font-weight: bold;">
                        方法 3: 只提取主要内容
                    </div>
                    <iframe id="test3" style="flex: 1; border: none;"></iframe>
                </div>

                <!-- 测试 4: 完整文档 -->
                <div style="display: flex; flex-direction: column; background: white; border-radius: 8px; overflow: hidden;">
                    <div style="padding: 10px; background: #4facfe; color: white; font-size: 12px; font-weight: bold;">
                        方法 4: 完整 HTML 文档
                    </div>
                    <iframe id="test4" style="flex: 1; border: none;"></iframe>
                </div>
            </div>

            <div style="padding: 10px; background: #ffd700; border-radius: 4px; font-size: 12px;">
                <strong>💡 检查指南:</strong><br>
                1. 查看上方 4 个测试框，哪个显示了内容？<br>
                2. 如果都是白屏，查看控制台的详细日志<br>
                3. 告诉我哪个方法有效（如果有的话）
            </div>
        </div>
    `;

    const statusBox = document.getElementById('status-box');

    function updateStatus(msg) {
        statusBox.innerHTML += '<br>' + msg;
        console.log(msg);
    }

    // 步骤 3: 准备不同的内容版本
    console.log('[步骤 3] 准备测试内容...\n');

    // 版本 1: 原始内容
    const version1 = realContent;

    // 版本 2: 使用 fixHtmlContent 处理
    const baseUrl = testUrl;
    const version2 = fixHtmlContent(realContent, baseUrl);

    // 版本 3: 简化版 - 只提取 body 内容
    const version3 = `<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <style>
        body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; padding: 20px; line-height: 1.8; }
        img { max-width: 100%; height: auto; }
    </style>
</head>
<body>
    ${realContent}
</body>
</html>`;

    // 版本 4: 完整文档格式
    const version4 = `<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <style>
        * { box-sizing: border-box; }
        html, body {
            background-color: #ffffff !important;
            color: #333333 !important;
            font-family: -apple-system, BlinkMacSystemFont, 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', sans-serif !important;
            font-size: 16px !important;
            line-height: 1.8 !important;
            padding: 16px !important;
            margin: 0 !important;
        }
        img { max-width: 100% !important; height: auto !important; display: block !important; margin: 10px 0 !important; }
        p { margin: 12px 0 !important; }
        a { color: #1a73e8 !important; }
        .rich_media_content { color: #333333 !important; }
    </style>
</head>
<body>
    ${realContent}
</body>
</html>`;

    // 步骤 4: 加载内容到 iframe
    console.log('[步骤 4] 加载内容到 iframe...\n');

    function loadIframe(iframeId, content, versionName) {
        const iframe = document.getElementById(iframeId);
        updateStatus(`<br><strong>加载 ${versionName}...</strong>`);

        iframe.src = 'about:blank';
        iframe.onload = () => {
            try {
                const doc = iframe.contentDocument || iframe.contentWindow.document;
                doc.open();
                doc.write(content);
                doc.close();

                setTimeout(() => {
                    try {
                        const bodyLength = doc.body?.innerHTML?.length || 0;
                        updateStatus(`  ✅ 内容已写入 (${bodyLength} 字符)`);

                        // 检查是否有可见内容
                        const textContent = doc.body?.textContent || '';
                        if (textContent.trim().length > 50) {
                            updateStatus(`  ✅ 有可见内容 (${textContent.trim().length} 字符)`);
                        } else {
                            updateStatus(`  ⚠️ 内容可能为空或不可见`);
                        }
                    } catch (e) {
                        updateStatus(`  ⚠️ 无法验证内容: ${e.message}`);
                    }
                }, 100);
            } catch (e) {
                updateStatus(`  ❌ 失败: ${e.message}`);
            }
        };
    }

    // 并行加载所有测试
    updateStatus('<br><strong>========================================</strong>');
    updateStatus('<strong>开始并行测试...</strong>');

    loadIframe('test1', version1, '方法 1');
    await new Promise(r => setTimeout(r, 200));

    loadIframe('test2', version2, '方法 2');
    await new Promise(r => setTimeout(r, 200));

    loadIframe('test3', version3, '方法 3');
    await new Promise(r => setTimeout(r, 200));

    loadIframe('test4', version4, '方法 4');

    // 步骤 5: 等待并总结
    setTimeout(() => {
        updateStatus('<br><strong>========================================</strong>');
        updateStatus('<strong>🔍 检查结果:</strong>');
        updateStatus('- 查看上方 4 个测试框');
        updateStatus('- 告诉我哪个显示了内容');
        updateStatus('<br><strong>恢复按钮:</strong> 在控制台输入 restoreOriginalContent()');

        console.log('\n✅ 测试完成！');
        console.log('📊 请检查上方 4 个 iframe：');
        console.log('   方法 1: 原始 HTML');
        console.log('   方法 2: fixHtmlContent 处理');
        console.log('   方法 3: 简化版');
        console.log('   方法 4: 完整文档');
        console.log('\n告诉我哪个显示了内容！');

    }, 3000);

    // 提供恢复函数
    window.restoreOriginalContent = () => {
        detail.innerHTML = originalContent;
        detail.classList.remove('webview-mode');
        console.log('✅ 已恢复原始内容');
    };

})();
