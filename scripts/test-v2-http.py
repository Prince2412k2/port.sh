#!/usr/bin/env python3
"""HTTP acceptance checks against a running backend with real map data."""
import json
import re
import struct
import sys
import urllib.error
import urllib.request

base = sys.argv[1].rstrip("/")

def get(path, headers=None):
    try:
        return urllib.request.urlopen(urllib.request.Request(base+path,headers=headers or {}),timeout=15)
    except urllib.error.HTTPError as response:
        return response

with get('/api/v2/bootstrap') as response:
    assert response.status == 200
    assert response.headers['Content-Type'].startswith('application/json')
    etag = response.headers['ETag']
    assert json.load(response)['protocol'] == 2
with get('/api/v2/bootstrap',{'If-None-Match':etag}) as response:
    assert response.status == 304
    assert response.headers['ETag'] == etag
with get('/api/v2/map') as response:
    catalogue = json.load(response)
    assert catalogue['available'], 'Mount the real map archive first'
asset = catalogue['base']+'/vector.pmtiles'
with get(asset,{'Range':'bytes=0-126'}) as response:
    assert response.status == 206
    assert response.read().startswith(b'PMTiles\x03')
    assert response.headers['Content-Range'].startswith('bytes 0-126/')
    assert 'immutable' in response.headers['Cache-Control']
    etag = response.headers['ETag']
with get(asset,{'Range':'bytes=0-126','If-Range':etag}) as response:
    assert response.status == 206
    assert len(response.read()) == 127
with get(asset,{'If-None-Match':etag}) as response:
    assert response.status == 304
with get(catalogue['base']+'/tiles/12/2873/1778') as response:
    assert response.status == 200
    assert 0 < len(response.read()) < 8*1024*1024
with get(catalogue['base']+'/tiles/255/0/0') as response:
    assert response.status == 400
assert catalogue['terrain_format']=='tmhg-tiles-v1'
with get(catalogue['base']+'/terrain/12/2873/1778') as response:
    terrain=response.read();assert terrain[:5]==b'TMHG\x01'
    assert len(terrain)==48+49*49*2
    assert struct.unpack_from('<II',terrain,40)==(49,49)
    assert 'immutable' in response.headers['Cache-Control']
with get('/v2/') as response:
    page = response.read().decode()
    bundle = re.search(r'build/([a-f0-9]{64})/host\.js',page).group(1)
    assert response.headers['Cache-Control'] == 'no-cache'
for file in ['host.js','worker.js','portfolio_v2_browser.js','portfolio_v2_browser_bg.wasm']:
    with get(f'/v2/build/{bundle}/{file}') as response:
        assert response.status == 200
        assert 'immutable' in response.headers['Cache-Control']
        if file.endswith('.js'): assert b'__V2_BUILD__' not in response.read()
        else: assert response.read(4) == b'\0asm'
print('PASS: bootstrap/conditional GET, immutable ranges/If-Range, MVT delivery/bounds, small elevation tiles, coherent fingerprinted web bundle')
