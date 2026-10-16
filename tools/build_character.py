"""Package the approved PixelLab sources into runtime atlases and Windows icons.

Offline and deterministic: this does not generate artwork or contact a provider.
Only a public edition with its explicit identity adapter can be written.
"""
from pathlib import Path
import argparse
import hashlib
import json
import re
import struct
from io import BytesIO
from PIL import Image

STATES = ['idle', 'hover', 'dragging', 'falling', 'thinking', 'working',
          'searching_web', 'looking_at_screen', 'using_pc', 'waiting_permission',
          'waiting_user', 'success', 'error', 'privacy', 'dnd', 'sleeping']
BADGES = {'waiting_permission': 'warning', 'success': 'success', 'error': 'error',
          'privacy': 'privacy', 'dnd': 'dnd'}
PREFIX = 'assets/pixellab/production/'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def png(image):
    out = BytesIO()
    image.save(out, format='PNG', optimize=False)
    return out.getvalue()


def package(root, check=False):
    root = Path(root).resolve()
    identity = (root/'src/identity.ts').read_text(encoding='utf8')
    if not re.search(r'edition:\s*"public"', identity):
        raise ValueError('PUBLIC_EDITION_REQUIRED')
    source = root/'assets/character-source'
    provenance = json.loads((source/'provenance.json').read_text(encoding='utf8'))
    for path, expected in provenance['sourceFiles'].items():
        if sha((root/path).read_bytes()) != expected:
            raise ValueError('SOURCE_CHANGED: '+path)
    outputs = {}

    def put(path, data):
        outputs[path] = png(data) if isinstance(data, Image.Image) else data

    def text(path, value):
        put(path, (json.dumps(value, ensure_ascii=False, indent=2)+'\n').encode())

    original = Image.open(root/'assets/character-preview/pet-idle.png').convert('RGBA')
    assert original.size == (144, 192)
    reference = Image.new('RGBA', original.size)
    reference.paste(original, (0, 10))
    assert original.getchannel('A').getbbox()[3]+10 == 188
    put('assets/public/pet-reference.png', reference)
    runtime = json.loads((root/(PREFIX+'runtime.json')).read_text())
    runtime['pet'] = {}
    registry = []
    for state in STATES:
        job = provenance['animations'][state]
        raw = Image.open(source/(state+'.png')).convert('RGBA')
        count = job['generatedFrames']+1
        assert raw.size == (144*count, 192)
        # The provider's last frame is an interpolation, not an exact copy.
        # Preserve every generated frame and append the approved rest frame.
        frames = [reference.copy()]
        for index in range(1, count):
            frame = raw.crop((index*144, 0, (index+1)*144, 192))
            bounds = frame.getchannel('A').getbbox()
            assert bounds and 0 < bounds[0] and bounds[2] < 144
            assert 0 < bounds[1] and bounds[3]+10 < 192
            aligned = Image.new('RGBA', (144,192))
            aligned.paste(frame, (0,10))
            frames.append(aligned)
        frames.append(reference.copy())
        badge = None
        if state in BADGES:
            badge = Image.open(root/(PREFIX+'icons/'+BADGES[state]+'.png')).convert('RGBA')
        union = Image.new('L', (144,192))
        from PIL import ImageChops
        sheet = Image.new('RGBA', (144*len(frames),192))
        for index, frame in enumerate(frames):
            if badge is not None:
                frame.alpha_composite(badge, (108,4))
            sheet.paste(frame, (index*144,0))
            union = ImageChops.lighter(union, frame.getchannel('A'))
        bbox = union.getbbox()
        period = 150 if state in ('dragging','falling','success','error') else 190
        durations = [250] + [period]*job['generatedFrames'] + [250]
        if state == 'idle':
            durations[0], durations[-1] = 1100, 700
        if state == 'sleeping':
            durations = [600] + [400]*job['generatedFrames'] + [600]
        entry = {'source': 'pet/'+state+'.png', 'width':144, 'height':192,
                 'baseline':188, 'frames':len(frames), 'durations':durations,
                 'loop':state not in ('success','error'),
                 'hitRegion': {'x':bbox[0], 'y':bbox[1], 'width':bbox[2]-bbox[0], 'height':bbox[3]-bbox[1]}}
        if badge is not None:
            entry['badge'] = 'icons/'+BADGES[state]+'.png'
        runtime['pet'][state] = entry
        put(PREFIX+'pet/'+state+'.png', sheet)
        registry.append('    (\n        "'+state+'",\n        include_bytes!("../../'+PREFIX+'pet/'+state+'.png"),\n        '+str(len(frames))+',\n    ),')
    text(PREFIX+'runtime.json', runtime)
    badges = ['    (\n        "'+state+'",\n        include_bytes!("../../'+PREFIX+'icons/'+name+'.png"),\n    ),' for state,name in BADGES.items()]
    put('src-tauri/src/pet_sprites.rs', ('// Packaged from the approved public PixelLab sources by tools/build_character.py.\n'
        'pub const SPRITES: &[(&str, &[u8], usize)] = &[\n'+'\n'.join(registry)+'\n];\n'
        '#[cfg(test)]\npub const BADGES: &[(&str, &[u8])] = &[\n'+'\n'.join(badges)+'\n];\n').encode())
    portrait = Image.open(source/'portrait.png').convert('RGBA')
    assert portrait.size == (64,64)
    face = Image.new('RGBA', (72,72))
    face.paste(portrait, (4,4))
    sizes = [16,24,32,48,64,128,256]
    icons = {size:png(face.resize((size,size), Image.Resampling.NEAREST)) for size in sizes}
    # Write every ICO entry explicitly; Pillow's ICO writer otherwise resamples
    # its smaller entries differently from the tray/avatar PNGs.
    header = struct.pack('<HHH',0,1,len(sizes))
    offset = 6+16*len(sizes)
    entries = []
    for size in sizes:
        data = icons[size]
        entries.append(struct.pack('<BBBBHHII',size%256,size%256,0,0,1,32,len(data),offset))
        offset += len(data)
    put('src-tauri/icons/icon.ico', header+b''.join(entries)+b''.join(icons[size] for size in sizes))
    put('src-tauri/icons/icon.png', icons[256])
    put('src-tauri/icons/tray.png', icons[32])
    put('assets/branding/assistant-avatar.png', icons[64])
    # The user's original public compass was also generated with PixelLab.
    compass = Image.open(source/'user-compass.png').convert('RGBA')
    put('assets/branding/user-avatar.png', compass.resize((48,48),Image.Resampling.NEAREST))
    text('assets/branding/manifest.json', {
        'name':'PixelLab original public concierge',
        'generator':'tools/build_character.py; offline technical packaging, no new artwork',
        'provenance':'assets/character-source/provenance.json',
        'licenseNotice':'assets/character-source/NOTICE.md',
        'portraitPadding':4, 'iconResampling':'nearest-neighbor',
        'icoSizes':sizes,
        'files':{path:sha(data) for path,data in outputs.items() if path.startswith(('src-tauri/icons/','assets/branding/'))}
    })
    for path, data in outputs.items():
        target = root/path
        if check:
            if not target.exists() or target.read_bytes() != data:
                raise ValueError('GENERATED_ASSET_STALE: '+path)
        else:
            target.parent.mkdir(parents=True,exist_ok=True)
            target.write_bytes(data)
    return {'status':'PASS','states':16,'frames':sum(e['frames'] for e in runtime['pet'].values()),
            'outputs':len(outputs),'publicIdentity':True,'check':check}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    print(json.dumps(package(args.root,args.check)))
