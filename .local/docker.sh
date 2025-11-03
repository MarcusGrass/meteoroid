#/bin/bash
set -e
docker build . --build-arg "USERID=$UID" --build-arg "GROUPID=$(id -g)" -t meteoroid
WORKDIR="$1"
LOCAL="$2"
VANILLA="$3"
OUTPUT="$4"
LOCAL_CRATES_MOUNT=""
LOCAL_CRATES_ARG=""
if [ ! -z "${METEOROID_LOCAL+x}" ]; then
  LOCAL_CRATES_MOUNT="--mount type=bind,src=${METEOROID_LOCAL},dst=/data/crates"
  LOCAL_CRATES_ARG=" local -p /data/crates"
fi

shift 4
docker run -u $UID --rm \
$LOCAL_CRATES_MOUNT \
--mount type=bind,src="$WORKDIR",dst=/data/workdir \
--mount type=bind,src="$LOCAL",dst=/data/local \
--mount type=bind,src="$VANILLA",dst=/data/upstream \
--mount type=bind,src="$OUTPUT",dst=/data/output \
--mount type=bind,src="./target",dst=/app/target \
meteoroid "$@" $LOCAL_CRATES_ARG