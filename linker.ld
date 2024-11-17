PHDRS
{
  text PT_LOAD FLAGS(5);
  data PT_LOAD FLAGS(6);
  note PT_NOTE FLAGS(4);
}

SECTIONS {
    .custom_usdt_probes : {
        KEEP(*(.custom_usdt_probes))
    } :data
}