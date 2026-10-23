from pathlib import Path
import struct
# Original geometric F mark; 32-bit icon images include an AND mask for Windows.
def icon(size):
    data=bytearray()
    for y in reversed(range(size)):
        for x in range(size):
            channels=[0,0,0,0]
            for dy in range(4):
                for dx in range(4):
                    u=(x+(dx+.5)/4)/size; v=(y+(dy+.5)/4)/size
                    cx=max(.21,min(.79,u));cy=max(.21,min(.79,v))
                    inside=(u-cx)**2+(v-cy)**2<=.17**2
                    letter=(.29<=u<=.40 and .24<=v<=.77) or (.29<=u<=.72 and .24<=v<=.35) or (.29<=u<=.64 and .45<=v<=.56)
                    rgba=(255,255,255,255) if inside and letter else ((40,101,205,255) if inside else (0,0,0,0))
                    for i,n in enumerate(rgba):channels[i]+=n
            r,g,b,a=[n//16 for n in channels];data.extend([b,g,r,a])
    mask=bytes(((size+31)//32)*4*size)
    return struct.pack('<IiiHHIIiiII',40,size,size*2,1,32,0,len(data),0,0,0,0)+data+mask
sizes=[16,24,32,48,64,256];images=[icon(n) for n in sizes];offset=6+16*len(sizes)
header=struct.pack('<HHH',0,1,len(sizes));directory=bytearray()
for n,data in zip(sizes,images):
    directory+=struct.pack('<BBBBHHII',n%256,n%256,0,0,1,32,len(data),offset);offset+=len(data)
Path('assets').mkdir(exist_ok=True)
Path('assets/flaragon.ico').write_bytes(header+directory+b''.join(images))
