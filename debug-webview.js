/**
 * Webview 深度诊断工具
 * 在 Tauri 应用控制台中运行此脚本
 */

(async function diagnoseWebview() {
    console.clear();
    console.log('🔍 开始深度诊断...\n');

    const testUrl = 'https://mp.weixin.qq.com/s?__biz=MzU5NzUxNjg3Nw==&mid=2247507651&idx=2&sn=234da27242f990a240c72875f4bd6712&chksm=ffed9005cd531e9a433c97fc9231451684e4d6ebeaf0a82aecd0191d5c9cded143b4ab354a8e&scene=0#rd';

    // 步骤 1: 检查环境
    console.log('📋 [步骤 1] 环境检查');
    console.log('   - Tauri API:', typeof invoke !== 'undefined' ? '✅' : '❌');
    console.log('   - detail-content:', document.getElementById('detail-content') ? '✅' : '❌');
    console.log('   - 测试 URL:', testUrl.substring(0, 50) + '...\n');

    if (typeof invoke === 'undefined') {
        console.error('❌ Tauri API 不可用，请在 Tauri 应用中运行');
        return;
    }

    // 步骤 2: 获取内容
    console.log('🌐 [步骤 2] 获取网页内容...');
    let htmlContent = '';
    try {
        htmlContent = await invoke('fetch_website_content', { url: testUrl });
        console.log('✅ 成功获取内容');
        console.log('   - 长度:', htmlContent.length, '字符');
        console.log('   - 前 200 字符:', htmlContent.substring(0, 200));
        console.log('');
    } catch (e) {
        console.error('❌ 获取内容失败:', e);
        return;
    }

    // 步骤 3: 测试不同的加载方式
    console.log('🧪 [步骤 3] 测试不同的 iframe 加载方式\n');

    const detail = document.getElementById('detail-content');
    const originalContent = detail.innerHTML;

    // 创建测试容器
    detail.classList.add('webview-mode');
    detail.innerHTML = `
        <div style="display: flex; flex-direction: column; height: 100%; gap: 10px; padding: 10px; background: #f5f5f5;">
            <div id="test-status" style="padding: 10px; background: white; border-radius: 4px; font-family: monospace; font-size: 12px;">
                初始化测试...
            </div>
            <div style="flex: 1; display: flex; gap: 10px;">
                <div style="flex: 1; display: flex; flex-direction: column;">
                    <div style="padding: 5px; background: #ddd; text-align: center; font-size: 12px;">方法 1: srcdoc</div>
                    <iframe id="test1" style="flex: 1; border: 2px solid #ccc; background: white;"></iframe>
                </div>
                <div style="flex: 1; display: flex; flex-direction: column;">
                    <div style="padding: 5px; background: #ddd; text-align: center; font-size: 12px;">方法 2: data URI</div>
                    <iframe id="test2" style="flex: 1; border: 2px solid #ccc; background: white;"></iframe>
                </div>
                <div style="flex: 1; display: flex; flex-direction: column;">
                    <div style="padding: 5px; background: #ddd; text-align: center; font-size: 12px;">方法 3: src (about:blank)</div>
                    <iframe id="test3" style="flex: 1; border: 2px solid #ccc; background: white;"></iframe>
                </div>
            </div>
        </div>
    `;

    const statusEl = document.getElementById('test-status');
    const test1 = document.getElementById('test1');
    const test2 = document.getElementById('test2');
    const test3 = document.getElementById('test3');

    function updateStatus(msg) {
        statusEl.innerHTML += '<br>' + msg;
        console.log(msg);
    }

    updateStatus('✅ 测试环境已准备');

    // 创建简单的测试 HTML
    const simpleTestHtml = `<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <style>
        body {
            font-family: Arial, sans-serif;
            padding: 20px;
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            color: white;
            margin: 0;
        }
        .box {
            background: rgba(255,255,255,0.2);
            padding: 20px;
            border-radius: 8px;
            text-align: center;
        }
        h1 { margin: 0 0 10px 0; }
        p { margin: 5px 0; }
    </style>
</head>
<body>
    <div class="box">
        <h1>✅ 测试成功!</h1>
        <p>如果你能看到这个，说明此方法有效</p>
        <p>时间: ${new Date().toLocaleString()}</p>
    </div>
</body>
</html>`;

    // 测试 1: srcdoc
    updateStatus('<br><strong>[测试 1] srcdoc 方法...</strong>');
    try {
        test1.onload = () => {
            updateStatus('  ✅ onload 触发');
            setTimeout(() => {
                try {
                    const doc = test1.contentDocument || test1.contentWindow.document;
                    const hasContent = doc && doc.body && doc.body.innerHTML.length > 0;
                    updateStatus(hasContent ? '  ✅ 内容已加载' : '  ❌ 内容为空');
                } catch (e) {
                    updateStatus('  ⚠️ 跨域限制 (但可能已显示)');
                }
            }, 100);
        };
        test1.onerror = (e) => updateStatus('  ❌ onerror: ' + e);
        test1.srcdoc = simpleTestHtml;
        updateStatus('  ✓ srcdoc 已设置');
    } catch (e) {
        updateStatus('  ❌ 失败: ' + e.message);
    }

    // 测试 2: data URI
    updateStatus('<br><strong>[测试 2] data URI 方法...</strong>');
    try {
        test2.onload = () => {
            updateStatus('  ✅ onload 触发');
            setTimeout(() => {
                try {
                    const doc = test2.contentDocument || test2.contentWindow.document;
                    const hasContent = doc && doc.body && doc.body.innerHTML.length > 0;
                    updateStatus(hasContent ? '  ✅ 内容已加载' : '  ❌ 内容为空');
                } catch (e) {
                    updateStatus('  ⚠️ 跨域限制');
                }
            }, 100);
        };
        test2.onerror = (e) => updateStatus('  ❌ onerror: ' + e);
        const dataUri = 'data:text/html;charset=utf-8,' + encodeURIComponent(simpleTestHtml);
        test2.src = dataUri;
        updateStatus('  ✓ data URI 已设置');
    } catch (e) {
        updateStatus('  ❌ 失败: ' + e.message);
    }

    // 测试 3: about:blank + write
    updateStatus('<br><strong>[测试 3] about:blank + document.write...</strong>');
    try {
        test3.onload = () => {
            updateStatus('  ✅ onload 触发');
            setTimeout(() => {
                try {
                    const doc = test3.contentDocument || test3.contentWindow.document;
                    doc.open();
                    doc.write(simpleTestHtml);
                    doc.close();
                    updateStatus('  ✓ document.write 完成');
                    setTimeout(() => {
                        const hasContent = doc && doc.body && doc.body.innerHTML.length > 0;
                        updateStatus(hasContent ? '  ✅ 内容已加载' : '  ❌ 内容为空');
                    }, 50);
                } catch (e) {
                    updateStatus('  ❌ 失败: ' + e.message);
                }
            }, 100);
        };
        test3.src = 'about:blank';
        updateStatus('  ✓ about:blank 已设置');
    } catch (e) {
        updateStatus('  ❌ 失败: ' + e.message);
    }

    // 等待测试完成
    setTimeout(() => {
        updateStatus('<br><strong>========================================</strong>');
        updateStatus('<strong>🔍 检查上方三个 iframe：</strong>');
        updateStatus('- 如果能看到紫色渐变背景 = ✅ 该方法有效');
        updateStatus('- 如果是白色空白 = ❌ 该方法失败');
        updateStatus('<br><strong>下一步:</strong>');
        updateStatus('1. 确定哪个 iframe 显示了内容');
        updateStatus('2. 将成功的方法应用到实际代码中');
        updateStatus('<br><strong>恢复按钮:</strong> 在控制台输入恢复原始内容');
    }, 2000);

    // 提供恢复函数
    window.restoreContent = () => {
        detail.innerHTML = originalContent;
        detail.classList.remove('webview-mode');
        console.log('✅ 已恢复原始内容');
    };

    console.log('\n💡 提示:');
    console.log('   - 查看右侧面板中的三个测试 iframe');
    console.log('   - 告诉我哪个显示了紫色背景和内容');
    console.log('   - 输入 restoreContent() 可恢复原始内容\n');

})();
