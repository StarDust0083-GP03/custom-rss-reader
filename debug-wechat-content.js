/**
 * 调试：检查微信公众号内容格式
 */

(async function debugWeChatContent() {
    const testUrl = 'https://mp.weixin.qq.com/s?__biz=MzU5NzUxNjg3Nw==&mid=2247507651&idx=2&sn=234da27242f990a240c72875f4bd6712&chksm=ffed9005cd531e9a433c97fc9231451684e4d6ebeaf0a82aecd0191d5c9cded143b4ab354a8e&scene=0#rd';

    console.log('🔍 获取微信公众号内容...\n');

    const content = await invoke('fetch_website_content', { url: testUrl });

    console.log('='.repeat(60));
    console.log('📊 内容分析报告');
    console.log('='.repeat(60));
    console.log('总长度:', content.length, '字符');
    console.log('');

    // 检查是否包含关键元素
    const checks = {
        'DOCTYPE声明': /<!DOCTYPE/i.test(content),
        'html标签': /<html/i.test(content),
        'body标签': /<body/i.test(content),
        'head标签': /<head/i.test(content),
        'rich_media_content': /rich_media_content/i.test(content),
        'data-content属性': /data-content/i.test(content),
        'script标签': /<script/i.test(content),
        'style标签': /<style/i.test(content),
        'img标签': /<img/i.test(content),
        '空内容': content.trim().length === 0
    };

    console.log('内容检查:');
    for (const [key, value] of Object.entries(checks)) {
        console.log(`  ${key}: ${value ? '✅' : '❌'}`);
    }
    console.log('');

    // 显示前 1000 字符
    console.log('前 1000 字符:');
    console.log('-'.repeat(60));
    console.log(content.substring(0, 1000));
    console.log('');
    console.log('...');

    // 显示后 500 字符
    console.log('');
    console.log('后 500 字符:');
    console.log('-'.repeat(60));
    console.log(content.substring(Math.max(0, content.length - 500)));
    console.log('');

    // 检查是否只是片段
    if (!/<html/i.test(content) && !/<body/i.test(content)) {
        console.log('⚠️ 警告: 内容似乎是 HTML 片段（不是完整文档）');
    }

    if (content.length < 500) {
        console.log('⚠️ 警告: 内容非常短，可能获取失败');
    }

    console.log('');
    console.log('='.repeat(60));

})();
