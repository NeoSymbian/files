#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# FA12 fixtures, our own content: ffmpeg's testsrc2 pattern with a frame counter and a 440 Hz tone, 2 s at 320x240,
# one file per container and codec pair, plus ffmpeg's own decode of each video track as SHA-256 per frame
# (*.frames: the decoders are bit-exact, so the tests compare hashes). Rerun after changing it; the files are committed.
set -eu
cd "$(dirname "$0")"
V="testsrc2=s=320x240:rate=25:duration=2"
A="sine=frequency=440:sample_rate=48000:duration=2"
ff() { ffmpeg -hide_banner -loglevel error -y "$@"; }
# H.264 High with B-frames and AAC-LC in MP4, the phone's usual
ff -f lavfi -i "$V" -f lavfi -i "$A" -c:v libx264 -profile:v high -preset medium -g 12 -bf 2 -pix_fmt yuv420p \
	-c:a aac -b:a 96k -ac 1 -movflags +faststart h264-aac.mp4
# the same in Matroska, moov-less
ff -i h264-aac.mp4 -c copy h264-aac.mkv
# H.264 Baseline in 3GP with its index at the end (no faststart)
ff -f lavfi -i "$V" -f lavfi -i "$A" -c:v libx264 -profile:v baseline -g 25 -pix_fmt yuv420p -c:a aac -ac 1 -ar 16000 \
	h264-baseline.3gp
# WebM: VP8 with Vorbis, VP9 with Opus
ff -f lavfi -i "$V" -f lavfi -i "$A" -c:v libvpx -b:v 400k -g 12 -c:a vorbis -strict -2 -ac 2 vp8-vorbis.webm
ff -f lavfi -i "$V" -f lavfi -i "$A" -c:v libvpx-vp9 -b:v 300k -g 12 -c:a libopus -b:a 64k vp9-opus.webm
# what Files names but doesn't play: H.263 in 3GP
ff -f lavfi -i "testsrc2=s=176x144:rate=15:duration=1" -c:v h263 h263.3gp
for f in h264-aac.mp4 h264-aac.mkv h264-baseline.3gp vp8-vorbis.webm vp9-opus.webm; do
	ff -i "$f" -map 0:v -f framehash -hash sha256 -pix_fmt yuv420p - | grep -v '^#' | awk -F', *' '{print $6}' > "$f.frames"
done
ls -l
