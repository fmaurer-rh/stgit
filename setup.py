#!/usr/bin/env python
# -*- coding: utf-8 -*-
from __future__ import (absolute_import, division, print_function,
                        unicode_literals)
from distutils.core import setup
from glob import glob
import os
import sys

from stgit import commands, version
from stgit.completion.bash import write_bash_completion


def __version_to_list(version):
    """Convert a version string to a list of numbers or strings
    """
    ver_list = []
    for p in version.split('.'):
        try:
            n = int(p)
        except ValueError:
            n = p
        ver_list.append(n)
    return ver_list


def __check_min_version(min_ver, ver):
    """Check whether ver is greater or equal to min_ver
    """
    min_ver_list = __version_to_list(min_ver)
    ver_list = __version_to_list(ver)
    return min_ver_list <= ver_list


def __check_python_version():
    """Check the minimum Python version
    """
    pyver = '.'.join(map(str, sys.version_info))
    if not __check_min_version(version.python_min_ver, pyver):
        print('Python version %s or newer required. Found %s'
              % (version.python_min_ver, pyver), file=sys.stderr)
        sys.exit(1)


def __check_git_version():
    """Check the minimum GIT version
    """
    from stgit.run import Run
    gitver = Run('git', '--version').output_one_line().split()[2]
    if not __check_min_version(version.git_min_ver, gitver):
        print('GIT version %s or newer required. Found %s'
              % (version.git_min_ver, gitver), file=sys.stderr)
        sys.exit(1)


# Check the minimum versions required
__check_python_version()
__check_git_version()

# ensure readable template files
old_mask = os.umask(0o022)

for get_ver in [
    version.git_describe_version,
    version.git_archival_version,
    version.get_builtin_version,
]:
    try:
        ver = get_ver()
    except version.VersionUnavailable:
        continue
    else:
        break
else:
    print('StGit version unavailable', file=sys.stderr)
    sys.exit(1)

with open('stgit/builtin_version.py', 'w') as f:
    print(
        '# This file is automatically generated. Do not edit.',
        'version = {ver!r}'.format(ver=ver),
        sep='\n',
        file=f,
    )

# generate the python command list
with open('stgit/commands/cmdlist.py', 'w') as f:
    commands.py_commands(commands.get_commands(allow_cached=False), f)

if not os.path.exists('completion'):
    os.mkdir('completion')

# generate the bash completion script
with open(os.path.join('completion', 'stgit.bash'), 'w') as f:
    write_bash_completion(f)

setup(
    name='stgit',
    version=ver,
    license='GPLv2',
    author='Catalin Marinas',
    author_email='catalin.marinas@gmail.com',
    url='http://www.procode.org/stgit/',
    download_url='https://repo.or.cz/stgit.git',
    description='Stacked GIT',
    long_description='Push/pop utility on top of GIT',
    scripts=['stg'],
    packages=list(map(str, ['stgit', 'stgit.commands', 'stgit.lib'])),
    data_files=[
        ('share/stgit/templates', glob('stgit/templates/*.tmpl')),
        ('share/stgit/examples', glob('examples/*.tmpl')),
        ('share/stgit/examples', ['examples/gitconfig']),
        ('share/stgit/contrib', ['contrib/stgbashprompt.sh']),
        ('share/stgit/completion', ['completion/stgit.bash']),
    ],
    package_data={
        'stgit': [
            'templates/covermail.tmpl',
            'templates/mailattch.tmpl',
            'templates/patchandattch.tmpl',
            'templates/patchexport.tmpl',
            'templates/patchmail.tmpl',
        ],
    },
    classifiers=[
        'Development Status :: 5 - Production/Stable',
        'Environment :: Console',
        'Intended Audience :: Developers',
        'License :: OSI Approved :: GNU General Public License v2 (GPLv2)'
        'Natural Language :: English',
        'Operating System :: OS Independent',
        'Programming Language :: Python',
        'Programming Language :: Python :: 2',
        'Programming Language :: Python :: 2.6',
        'Programming Language :: Python :: 2.7',
        'Programming Language :: Python :: 3',
        'Programming Language :: Python :: 3.3',
        'Programming Language :: Python :: 3.4',
        'Programming Language :: Python :: 3.5',
        'Programming Language :: Python :: 3.6',
        'Programming Language :: Python :: 3.7',
        'Programming Language :: Python :: Implementation :: CPython',
        'Programming Language :: Python :: Implementation :: PyPy',
        'Topic :: Software Development :: Version Control',
    ],
)

# restore the old mask
os.umask(old_mask)
