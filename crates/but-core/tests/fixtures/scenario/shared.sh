
# Just to save a line in the scripts that use this module
set -eu -o pipefail

function remote_tracking_caught_up() {
  setup_remote_tracking "$1"
}

function setup_remote_tracking() {
  local branch_name="${1:?}"
  local remote_branch_name=${2:-"$branch_name"}
  local mode=${3:-"cp"}
  mkdir -p .git/refs/remotes/origin

  if [[ "$mode" == "cp" ]]; then
    cp ".git/refs/heads/$branch_name" ".git/refs/remotes/origin/$remote_branch_name"
  else
    mv ".git/refs/heads/$branch_name" ".git/refs/remotes/origin/$remote_branch_name"
  fi
}

function turn_into_remote_branch() {
  setup_remote_tracking "$@" "move"
}

function tick () {
  if test -z "${tick+set}"; then
    tick=1675176957
  else
    tick=$(($tick + 60))
  fi
  GIT_COMMITTER_DATE="$tick +0100"
  GIT_AUTHOR_DATE="$tick +0100"
  export GIT_COMMITTER_DATE GIT_AUTHOR_DATE
}

function tick_committer () {
  if test -z "${tick+set}"; then
    tick=1675176957
  else
    tick=$(($tick + 60))
  fi
  GIT_COMMITTER_DATE="$tick +0100"
  export GIT_COMMITTER_DATE
}

function setup_target_to_match_main() {
  remote_tracking_caught_up main

  cat <<EOF >>.git/config
[remote "origin"]
	url = ./fake/local/path/which-is-fine-as-we-dont-fetch-or-push
	fetch = +refs/heads/*:refs/remotes/origin/*
EOF
}


function add_main_remote_setup() {
  cat <<EOF >>.git/config
[remote "origin"]
	url = ./fake/local/path/which-is-fine-as-we-dont-fetch-or-push
	fetch = +refs/heads/*:refs/remotes/origin/*

[branch "main"]
  remote = "origin"
  merge = refs/heads/main
EOF
}

function commit() {
  local message=${1:?first argument is the commit message}
  git commit -m "$message" --allow-empty
}

function commit-file() {
  local name="${1:?First argument is the filename}"
  local content=${2:-$1}
  echo $content >$name && git add $name && git commit -m "add $content"
}

function write_ref_safely() {
  cat >.git/tmp
  mv .git/tmp "${1:?first argument is the full path to the ref to write}"
}