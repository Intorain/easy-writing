# -*- mode: python ; coding: utf-8 -*-
from PyInstaller.utils.hooks import collect_submodules
from PyInstaller.utils.hooks import collect_all
from PyInstaller.utils.hooks import copy_metadata

datas = [('tool-contracts.json', '.')]
binaries = []
hiddenimports = []
datas += copy_metadata('langchain-core')
datas += copy_metadata('langgraph-checkpoint')
datas += copy_metadata('langchain-openai')
hiddenimports += collect_submodules('langgraph')
hiddenimports += collect_submodules('langchain')
hiddenimports += collect_submodules('langchain_openai')
tmp_ret = collect_all('deepagents')
datas += tmp_ret[0]; binaries += tmp_ret[1]; hiddenimports += tmp_ret[2]

# 分词表随包：SDK 估算用量时要用 tiktoken 的编码表，缺了会在作者机器上联网下载，离线时第一轮就失败。
# 构建机上取一次放进 tiktoken_cache/，worker 启动时指向它（见 worker.py 顶部）。
import os
import tiktoken
tiktoken_cache = os.path.join(SPECPATH, 'build', 'tiktoken_cache')
os.makedirs(tiktoken_cache, exist_ok=True)
os.environ['TIKTOKEN_CACHE_DIR'] = tiktoken_cache
tiktoken.get_encoding('cl100k_base')
datas += [(tiktoken_cache, 'tiktoken_cache')]
hiddenimports += ['tiktoken_ext', 'tiktoken_ext.openai_public']


a = Analysis(
    ['worker.py'],
    pathex=[],
    binaries=binaries,
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    noarchive=False,
    optimize=0,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name='ew-agent-runtime',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    console=True,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
)
coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=True,
    upx_exclude=[],
    name='ew-agent-runtime',
)
