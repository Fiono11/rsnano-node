#!/usr/bin/env python3
"""Build the complete v1.3 specification; requires tectonic and pypdf."""
from pathlib import Path
import hashlib
import subprocess
import tempfile
from pypdf import PdfReader

ROOT = Path(__file__).resolve().parents[2]

def main():
    source = Path(__file__).with_name('fast-archipelago.tex')
    with tempfile.TemporaryDirectory(prefix='fast-archipelago-') as work:
        subprocess.run(['tectonic', '--outdir', work, str(source)], check=True)
        built = Path(work) / 'fast-archipelago.pdf'
        result = PdfReader(built)
        text = '\n'.join(page.extract_text() for page in result.pages)
        assert 'v1.3' in text and 'conditional termination' in text
        output = ROOT / 'Fast_Archipelago.pdf'
        output.write_bytes(built.read_bytes())
        print(f'Wrote {output}: {len(result.pages)} pages, complete v1.3 source.')
    print('SHA-256:', hashlib.sha256(output.read_bytes()).hexdigest())

if __name__ == '__main__':
    main()
