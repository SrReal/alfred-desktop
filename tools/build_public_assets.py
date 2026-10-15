"""Reproducible geometric development pack; never reads personal artwork."""
from pathlib import Path
import argparse, hashlib, json
from PIL import Image, ImageDraw

STATES = ['idle', 'hover', 'dragging', 'falling', 'thinking', 'working', 'searching_web', 'looking_at_screen', 'using_pc', 'waiting_permission', 'waiting_user', 'success', 'error', 'privacy', 'dnd', 'sleeping']
INK = '#243a49'
CYAN = '#70d7d1'
BRASS = '#d5b470'
GLYPHS = {
    'history': [('circle', [12,12,8]), ('line',[12,7,12,12,16,14])],
    'commands': [('line',[5,6,10,12,5,18]), ('line',[13,18,20,18])],
    'tasks': [('rect',[5,4,19,20]), ('line',[8,9,10,11,15,7]), ('line',[8,15,16,15])],
    'activity': [('line',[3,12,7,12,10,5,14,19,17,12,21,12])],
    'startup': [('line',[6,16,12,5,18,16]), ('line',[6,20,18,20])],
    'dnd': [('circle',[12,12,8]), ('line',[6,18,18,6])],
    'calendar': [('rect',[4,6,20,20]), ('line',[4,10,20,10]), ('line',[8,3,8,7]), ('line',[16,3,16,7])],
    'mail': [('rect',[3,6,21,19]), ('line',[3,7,12,14,21,7])],
    'storage': [('rect',[4,5,20,20]), ('line',[4,10,20,10]), ('line',[16,15,17,15])],
    'connection': [('circle',[6,7,3]), ('circle',[18,17,3]), ('line',[9,7,15,7,15,17])],
    'warning': [('line',[12,3,22,21,2,21,12,3]), ('line',[12,9,12,14]), ('line',[12,17,12,18])],
    'error': [('circle',[12,12,9]), ('line',[8,8,16,16]), ('line',[16,8,8,16])],
    'success': [('circle',[12,12,9]), ('line',[6,12,10,16,18,8])],
    'attachment': [('line',[8,16,16,8,19,8,20,11,10,21,6,21,3,18,3,14,15,2])],
    'image': [('rect',[3,4,21,20]), ('circle',[16,8,2]), ('line',[4,18,9,12,13,16,16,13,20,18])],
    'capture': [('rect',[5,6,19,18]), ('line',[2,8,2,2,8,2]), ('line',[16,22,22,22,22,16])],
    'send': [('line',[3,3,21,12,3,21,7,12,3,3]), ('line',[7,12,21,12])],
    'search': [('circle',[10,10,6]), ('line',[15,15,21,21])],
    'add': [('line',[12,4,12,20]), ('line',[4,12,20,12])],
    'remove': [('line',[4,12,20,12])],
    'close': [('line',[5,5,19,19]), ('line',[19,5,5,19])],
    'minimize': [('line',[4,18,20,18])],
    'maximize': [('rect',[4,4,20,20])],
    'restore': [('rect',[3,8,16,21]), ('line',[8,8,8,3,21,3,21,16,16,16])],
    'back': [('line',[14,4,6,12,14,20])],
    'next': [('line',[10,4,18,12,10,20])],
    'refresh': [('line',[20,7,15,3,8,3,3,8,3,15,8,20,15,20,20,15]), ('line',[15,8,21,8,21,2])],
    'stop': [('rect',[5,5,19,19])],
    'play': [('line',[7,3,21,12,7,21,7,3])],
    'microphone': [('rect',[8,3,16,15]), ('line',[4,11,4,16,8,19,16,19,20,16,20,11]), ('line',[12,19,12,23])],
    'audio': [('line',[3,9,7,9,12,4,12,20,7,15,3,15,3,9]), ('line',[16,8,19,12,16,16]), ('line',[19,4,23,12,19,20])],
    'user': [('circle',[12,7,4]), ('line',[4,21,5,16,9,13,15,13,19,16,20,21])],
    'folder': [('line',[2,6,9,6,12,9,22,9,22,21,2,21,2,6])],
    'file': [('line',[5,2,15,2,20,7,20,22,5,22,5,2]), ('line',[15,2,15,7,20,7]), ('line',[8,12,16,12]), ('line',[8,17,16,17])],
    'terminal': [('rect',[2,4,22,21]), ('line',[5,8,9,12,5,16]), ('line',[12,16,19,16])],
    'browser': [('circle',[12,12,9]), ('line',[3,12,21,12]), ('line',[12,3,8,8,8,16,12,21,16,16,16,8,12,3])],
    'plugin': [('rect',[5,7,19,17]), ('line',[8,3,8,7]), ('line',[16,3,16,7]), ('line',[12,17,12,22])],
    'memory': [('rect',[5,5,19,19]), ('rect',[9,9,15,15]), ('line',[2,8,5,8]), ('line',[19,16,22,16])],
    'security': [('line',[12,2,21,6,20,15,12,22,4,15,3,6,12,2]), ('line',[7,12,11,16,17,8])],
    'chat': [('line',[3,3,21,3,21,17,10,17,4,22,4,17,3,17,3,3])],
    'settings': [('circle',[12,12,4]), ('circle',[12,12,8]), ('line',[12,1,12,4]), ('line',[12,20,12,23]), ('line',[1,12,4,12]), ('line',[20,12,23,12])],
    'privacy': [('rect',[4,10,20,22]), ('line',[7,10,7,5,10,2,14,2,17,5,17,10]), ('line',[12,14,12,18])],
}

def icon(name):
    image = Image.new('RGBA', (24,24))
    draw = ImageDraw.Draw(image)
    for kind, values in GLYPHS[name]:
        if kind == 'line':
            draw.line(list(zip(values[::2], values[1::2])), fill=CYAN, width=2)
        elif kind == 'rect':
            draw.rectangle(values, outline=CYAN, width=2)
        else:
            x,y,r = values
            draw.ellipse((x-r,y-r,x+r,y+r), outline=CYAN, width=2)
    return image

def generate(output):
    output = Path(output).resolve()
    repository = Path(__file__).resolve().parents[1]
    if output == repository:
        raise ValueError('OUTPUT_MUST_NOT_REPLACE_THE_PERSONAL_EDITION')
    def put(name, image):
        path = output / name
        path.parent.mkdir(parents=True, exist_ok=True)
        image.save(path)
    def text(name, value):
        path = output / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value, encoding='utf8')
    prefix = 'assets/pixellab/production/'
    icons = {}
    for name in GLYPHS:
        put(prefix + 'icons/' + name + '.png', icon(name))
        icons[name] = 'icons/' + name + '.png'
    def neutral(pulse=False):
        image = Image.new('RGBA', (144,192))
        draw = ImageDraw.Draw(image)
        draw.polygon([(72,28),(104,60),(104,138),(80,162),(80,184),(64,184),(64,162),(40,138),(40,60)], fill=INK)
        color = '#c1fff4' if pulse else CYAN
        draw.line([(72,28),(104,60),(104,138),(80,162),(80,184),(64,184),(64,162),(40,138),(40,60),(72,28)], fill=color, width=2)
        draw.line([(52,132),(72,65),(92,132)], fill=color, width=5)
        draw.line([(60,111),(84,111)], fill=BRASS, width=4)
        draw.rectangle((60,184,84,187), fill=BRASS)
        return image
    reference = neutral()
    put('assets/public/pet-reference.png', reference)
    text('assets/public/pet-source.svg', '<svg xmlns="http://www.w3.org/2000/svg" width="144" height="192" viewBox="0 0 144 192"><path d="M72 28L104 60V138L80 162V184H64V162L40 138V60Z" fill="#243a49" stroke="#70d7d1" stroke-width="2"/><path d="M52 132L72 65L92 132M60 111H84" fill="none" stroke="#70d7d1" stroke-width="5"/><path d="M60 186H84" stroke="#d5b470" stroke-width="4"/></svg>\n')
    badges = {'waiting_permission':'warning','success':'success','error':'error','privacy':'privacy','dnd':'dnd'}
    pet = {}
    for name in STATES:
        sheet = Image.new('RGBA', (432,192))
        for index, pulse in enumerate([False, True, False]):
            frame = neutral(pulse)
            if name in badges:
                frame.alpha_composite(icon(badges[name]), (108,4))
            sheet.alpha_composite(frame, (index*144,0))
        put(prefix + 'pet/' + name + '.png', sheet)
        bbox = sheet.getchannel('A').crop((0,0,144,192)).getbbox()
        pet[name] = {'source':'pet/'+name+'.png','width':144,'height':192,'baseline':188,'frames':3,'durations':[1000,250,250],'loop':True,'hitRegion':{'x':bbox[0],'y':bbox[1],'width':bbox[2]-bbox[0],'height':bbox[3]-bbox[1]}}
        if name in badges:
            pet[name]['badge'] = 'icons/' + badges[name] + '.png'
    ui = {}
    for name in ['button','card','input','bubble','permission','toggle','badge','panel']:
        size = (48,32) if name != 'panel' else (192,192)
        image = Image.new('RGBA', size, '#111e28')
        draw = ImageDraw.Draw(image)
        draw.rectangle((0,0,size[0]-1,size[1]-1), outline='#3b6471', width=1)
        draw.line((3,2,size[0]-4,2), fill='#284653', width=1)
        put(prefix + 'ui/' + name + '.png', image)
        ui[name] = {'source':'ui/'+name+'.png','width':size[0],'height':size[1],'nineSlice':[4,4,4,4]}
    text(prefix + 'runtime.json', json.dumps({'pet':pet,'icons':icons,'ui':ui}, indent=2)+'\n')
    app_icon = Image.new('RGBA',(256,256))
    draw = ImageDraw.Draw(app_icon)
    draw.rounded_rectangle((8,8,248,248),radius=40,fill=INK,outline=CYAN,width=8)
    draw.line([(56,208),(128,48),(200,208)], fill=CYAN,width=14)
    draw.line([(84,152),(172,152)],fill=BRASS,width=12)
    put('src-tauri/icons/icon.png',app_icon)
    put('src-tauri/icons/tray.png',app_icon.resize((32,32),Image.Resampling.LANCZOS))
    path = output / 'src-tauri/icons/icon.ico'
    app_icon.save(path, format='ICO', sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)])
    put('assets/branding/user-avatar.png',icon('user'))
    files = {name:hashlib.sha256((output/name).read_bytes()).hexdigest() for name in ['src-tauri/icons/icon.png','src-tauri/icons/icon.ico','src-tauri/icons/tray.png']}
    text('assets/branding/manifest.json',json.dumps({'name':'Original geometric development identity','generator':'tools/build_public_assets.py; geometric primitives only, no private reference image','files':files},indent=2)+'\n')
    entries = ',\n'.join('    ("'+name+'", include_bytes!("../../'+prefix+'pet/'+name+'.png"), 3)' for name in STATES)
    badge_entries = ',\n'.join('    ("'+state+'", include_bytes!("../../'+prefix+'icons/'+name+'.png"))' for state,name in badges.items())
    text('src-tauri/src/pet_sprites.rs', '// Generated from the original geometric public development pack.\npub const SPRITES: &[(&str, &[u8], usize)] = &[\n'+entries+'\n];\n#[cfg(test)]\npub const BADGES: &[(&str, &[u8])] = &[\n'+badge_entries+'\n];\n')
    text('assets/public/README.md', '# Original geometric development pack\n\nCreated reproducibly by `tools/build_public_assets.py` from geometric primitives. No personal artwork, reference character or provider output is read. This neutral development marker has a light pulse and state badges; it is not the final character design. Code, geometry and generated media in this pack are MIT, copyright (c) 2026 SrReal.\n')
    return {'sprites':len(STATES),'icons':len(GLYPHS),'ui':len(ui),'personalArtRead':False}

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out',required=True)
    args = parser.parse_args()
    print(json.dumps(generate(args.out)))
