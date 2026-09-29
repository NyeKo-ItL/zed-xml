<?xml version="1.0" encoding="UTF-8"?>
<!-- XSLT 3.0 stylesheet: xsl:mode, text value templates, XPath 3.1 in
     select/test/match attributes, output literal result elements with
     attribute value templates. -->
<xsl:stylesheet version="3.0"
    xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
    xmlns:xs="http://www.w3.org/2001/XMLSchema"
    xmlns:fn="urn:example:functions"
    exclude-result-prefixes="xs fn"
    expand-text="yes">

  <xsl:output method="html" html-version="5" indent="yes"/>
  <xsl:mode on-no-match="shallow-copy"/>
  <xsl:param name="threshold" as="xs:decimal" select="100"/>

  <xsl:function name="fn:label" as="xs:string">
    <xsl:param name="price" as="xs:decimal"/>
    <xsl:sequence select="if ($price gt $threshold) then 'expensive' else 'cheap'"/>
  </xsl:function>

  <xsl:template match="item[USPrice &gt; 0]">
    <li class="{fn:label(xs:decimal(USPrice))}" data-part="{@partNum}">{productName} costs {format-number(USPrice, '#,##0.00')}</li>
  </xsl:template>

  <xsl:template match="comment"/>
</xsl:stylesheet>
